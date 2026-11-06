//! Состояние монитора и его обход: раз в секунду читаем реестр, транскрипты, окна и процессы.
//! Здесь же запуск (очистка запросов, первый проход, --session) и снимок для --once.

use std::sync::mpsc;

use crate::cli::Options;
use crate::clock;
use crate::fixes::SettingsCache;
use crate::ideas::IdeaStore;
use crate::paths::Paths;
use crate::procs::Procs;
use crate::registry;
use crate::requests::{self, Requests};
use crate::session::Session;
use crate::text::clip_ansi;
use crate::ui::{Ui, View};

/// Всё, что монитор знает о сессиях и машине, без состояния интерфейса.
pub struct Monitor {
    pub paths: Paths,
    pub opts: Options,
    /// один снимок (--once или не терминал): окна и память не ждём в фоне
    pub once: bool,
    /// сессии в порядке появления в реестре; завершённые открытые вручную — в конце
    pub sessions: Vec<Session>,
    pub requests: Requests,
    pub procs: Procs,
    pub settings: SettingsCache,
    /// копилка идей агента по всем сессиям (~/.ccwatch/ideas.jsonl)
    pub ideas: IdeaStore,
}

impl Monitor {
    /// Номер сессии в списке по id.
    pub fn find(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// Сессия по id; без id или с неизвестным id — None.
    pub fn get(&self, id: Option<&str>) -> Option<&Session> {
        id.and_then(|id| self.find(id)).map(|i| &self.sessions[i])
    }

    /// Номера видимых сессий: живые и выбранная (даже если уже закрыта), новые сверху.
    pub fn visible(&self, sel: Option<&str>) -> Vec<usize> {
        let mut list: Vec<usize> = (0..self.sessions.len())
            .filter(|&i| {
                let s = &self.sessions[i];
                s.live || Some(s.id.as_str()) == sel
            })
            .collect();
        // сортировка стабильная: сессии с одинаковым временем запуска идут в порядке реестра
        list.sort_by(|&a, &b| started(&self.sessions[b]).total_cmp(&started(&self.sessions[a])));
        list
    }
}

/// Время запуска сессии из реестра; без реестра — 0.
fn started(s: &Session) -> f64 {
    s.reg.as_ref().map_or(0.0, |r| r.started_at)
}

/// Число из JSON, которое в JS проверялось бы как истинное: 0 и NaN — ложь.
fn truthy_num(x: f64) -> bool {
    x != 0.0 && !x.is_nan()
}

/// Полный id сессии: 36 символов, hex и дефисы (в нижнем регистре, как после to_lowercase).
fn is_full_id(q: &str) -> bool {
    q.len() == 36 && q.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

pub struct App {
    pub m: Monitor,
    pub ui: Ui,
    /// куда kill_session пишет итог завершения сессии в фоновом потоке
    pub(crate) kills_tx: mpsc::Sender<String>,
    pub(crate) kills_rx: mpsc::Receiver<String>,
}

impl App {
    /// Запуск: чистим недоставленные запросы прошлого запуска, первый проход и --session.
    pub fn start(paths: Paths, opts: Options, once: bool) -> App {
        let (kills_tx, kills_rx) = mpsc::channel();
        requests::clean_asks(&paths);
        let ideas = IdeaStore::load(&paths);
        let settings = SettingsCache::new(&paths);
        let wanted = opts.session.clone();
        let (plan, tips, skills) = (opts.plan, opts.tips, opts.skills);
        let mut app = App {
            m: Monitor {
                paths,
                opts,
                once,
                sessions: Vec::new(),
                requests: Requests::new(),
                procs: Procs::new(),
                settings,
                ideas,
            },
            ui: Ui::new(),
            kills_tx,
            kills_rx,
        };
        app.tick();
        if let Some(wanted) = wanted {
            app.open_wanted(&wanted, plan, tips, skills);
        }
        app
    }

    /// --session: открыть сессию по части имени или полному id (завершённую — прямо из транскрипта).
    fn open_wanted(&mut self, wanted: &str, plan: bool, tips: bool, skills: bool) {
        let q = wanted.to_lowercase();
        let mut found =
            self.m.sessions.iter().position(|x| x.id.starts_with(&q) || x.name().to_lowercase().contains(&q));
        if found.is_none() && is_full_id(&q) {
            let mut s = Session::new(&q);
            s.refresh(&self.m.paths, clock::now());
            s.subs_at = 0.0;
            s.refresh(&self.m.paths, clock::now() + 5000.0);
            self.m.sessions.push(s);
            found = Some(self.m.sessions.len() - 1);
        }
        if let Some(i) = found {
            self.ui.sel = Some(self.m.sessions[i].id.clone());
            self.ui.view = if plan {
                View::Plan
            } else if tips {
                View::Tips
            } else if skills {
                View::Skills
            } else {
                View::Detail
            };
        }
    }

    /// Один проход раз в секунду: хук, реестр, окна и память, обновление сессий, автопросьба о плане.
    pub fn tick(&mut self) {
        let now = clock::now();
        self.m.requests.check_hook(&self.m.paths, now);
        registry::scan(&self.m.paths, &mut self.m.sessions, self.m.opts.bell);
        if !self.m.once {
            // окно и память: сразу для новых сессий, дальше раз в 30 секунд
            let pids = self.live_pids();
            if pids.iter().any(|&p| !self.m.procs.knows(p)) || now - self.m.procs.at > 30e3 {
                self.m.procs.refresh(&pids);
            }
        }
        let sel = self.ui.sel.clone();
        let auto_ok = self.m.opts.auto_ask && !self.m.once && self.m.requests.hook_installed;
        for s in self.m.sessions.iter_mut() {
            if !s.live && Some(s.id.as_str()) != sel.as_deref() {
                continue;
            }
            s.refresh(&self.m.paths, now);
            // десктоп-сессии обычно сами заводят план в первые секунды, поэтому ждём полминуты работы
            let busy = s.reg.as_ref().is_some_and(|r| r.status == "busy");
            let since = match s.reg.as_ref() {
                Some(r) if truthy_num(r.status_updated_at) => r.status_updated_at,
                _ => now,
            };
            let busy_for = now - since;
            if auto_ok && s.live && busy && busy_for > 30e3 && s.plan.is_empty() && s.ask.is_none() && !s.auto_asked {
                s.auto_asked = true;
                self.m.requests.ask_plan(&self.m.paths, Some(s), true);
            }
        }
        let list = self.m.visible(self.ui.sel.as_deref());
        let sel_listed = list.iter().any(|&i| Some(self.m.sessions[i].id.as_str()) == self.ui.sel.as_deref());
        if !sel_listed {
            self.ui.sel = list.first().map(|&i| self.m.sessions[i].id.clone());
        }
        // новые идеи сессий — в копилку (повторно записанное не пишем)
        self.m.ideas.sync(&self.m.paths, &self.m.sessions);
    }

    /// Номера окон живых сессий с реестром — по ним ищем окна и память.
    fn live_pids(&self) -> Vec<u32> {
        self.m.sessions.iter().filter(|s| s.live).filter_map(|s| s.reg.as_ref().map(|r| r.pid)).collect()
    }

    /// Индекс выбранной сессии в списке сессий.
    pub fn sel_index(&self) -> Option<usize> {
        self.ui.sel.as_deref().and_then(|id| self.m.find(id))
    }

    /// Снимок для --once: окна и память берём сразу, без фона; строки кадра — как в консоли.
    pub fn snapshot(&mut self, w: i64) -> String {
        let pids = self.live_pids();
        self.m.procs.refresh(&pids);
        let frame = self.frame(w, 200);
        frame.body.iter().map(|l| clip_ansi(l, w)).collect::<Vec<_>>().join("\n")
    }

    /// Кадр живого режима: тело без последних двух строк, добитое пустыми до высоты, и подвал.
    pub fn screen(&mut self, w: i64, h: i64) -> Vec<String> {
        let f = self.frame(w, h);
        let keep = usize::try_from(h - 2).unwrap_or(0);
        let mut lines: Vec<String> = f.body.into_iter().take(keep).collect();
        while (lines.len() as i64) < h - 1 {
            lines.push(String::new());
        }
        lines.push(f.foot);
        lines.iter().map(|l| clip_ansi(l, w)).collect()
    }

    /// Итоги завершения сессий из фоновых потоков: показываем 15 секунд.
    pub fn poll_kills(&mut self) {
        while let Ok(msg) = self.kills_rx.try_recv() {
            self.ui.flash(msg, 15000.0);
        }
    }

    /// Выход: недоставленные запросы плана убираем, хук их уже не заберёт.
    pub fn quit(&mut self) {
        self.m.requests.drop_my_asks(&self.m.paths);
    }
}
