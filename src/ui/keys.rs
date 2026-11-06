//! Клавиши: разбор нажатий и переходы между экранами.
//! Русские буквы-двойники (й, з, ы, ш, ь, ч, ф, г, а, н, д) — те же клавиши, что латинские.

use std::sync::mpsc;

use crate::app::App;
use crate::clock;
use crate::procs;
use crate::ui::{Input, KillConfirm, SetConfirm, View};

/// Клавиша в терминах монитора.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    PageUp,
    PageDown,
    Home,
    End,
    CtrlC,
    CtrlU,
    Char(char),
    Other,
}

/// Нажата буква `c` (латинская; русский двойник проверяет вызывающий).
fn ch(k: Key, c: char) -> bool {
    k == Key::Char(c)
}

/// Подтверждение: y, н или д.
fn is_yes(k: Key) -> bool {
    matches!(k, Key::Char('y') | Key::Char('н') | Key::Char('д'))
}

impl App {
    /// Обрабатывает клавишу; true — выйти из монитора.
    pub fn on_key(&mut self, k: Key) -> bool {
        // набираем сообщение сессии: все клавиши, кроме Enter и Esc, идут в текст (q тоже)
        if self.ui.input.is_some() {
            return self.on_input_key(k);
        }
        // скрипт совета ждёт подтверждения: y — выполнить, любая другая клавиша — отмена
        if let Some(c) = self.ui.set_confirm.take() {
            if c.until > clock::now() && is_yes(k) {
                let msg = self.m.settings.run(&c.fix, &c.plan);
                self.ui.flash(msg, 12000.0);
            } else {
                self.ui.flash("Настройка не изменена.", 3000.0);
            }
            return false;
        }
        if k == Key::CtrlC || ch(k, 'q') || ch(k, 'й') {
            return true;
        }
        let up = k == Key::Up || ch(k, 'k');
        let down = k == Key::Down || ch(k, 'j');
        let open = matches!(k, Key::Enter | Key::Right) || ch(k, ' ') || ch(k, 'f') || ch(k, 'а');
        let back = matches!(k, Key::Esc | Key::Left | Key::Backspace);
        // ждём подтверждения завершения сессии: y — завершить, любая другая клавиша — отмена
        if let Some(c) = self.ui.confirm.take() {
            let found = self.m.find(&c.id);
            match found {
                Some(i) if c.until > clock::now() && is_yes(k) => {
                    self.ui.flash("Завершаю…", 10000.0);
                    let tx = self.kills_tx.clone();
                    procs::kill_session(
                        &self.m.sessions[i],
                        Box::new(move |msg| {
                            let _ = tx.send(msg);
                        }),
                    );
                }
                _ => self.ui.flash("Завершение отменено.", 3000.0),
            }
            return false;
        }
        if ch(k, 'p') || ch(k, 'з') {
            let i = self.sel_index();
            let s = match i {
                Some(i) => Some(&mut self.m.sessions[i]),
                None => None,
            };
            let msg = self.m.requests.ask_plan(&self.m.paths, s, false);
            self.ui.flash(msg, 5000.0);
            return false;
        }
        match self.ui.view {
            // сессию выбираем только в списке: в карточке стрелки ничего не переключают
            View::List => self.key_list(up, down, open),
            View::Detail => self.key_detail(k, open, back),
            View::Tips => self.key_tips(k, up, down, back),
            View::Skills => self.key_skills(k, up, down, back),
            View::Plan => self.key_plan(k, up, down, back),
            View::Ideas => self.key_ideas(k, up, down, back),
        }
        false
    }

    /// Набор сообщения сессии: текст, стирание, отправка по Enter, отмена по Esc.
    fn on_input_key(&mut self, k: Key) -> bool {
        match k {
            Key::CtrlC => return true,
            Key::Esc => {
                self.ui.input = None;
                self.ui.flash("Сообщение не отправлено.", 3000.0);
            }
            Key::Enter => {
                if let Some(input) = self.ui.input.take() {
                    let text = input.text.trim().to_string();
                    if let (false, Some(i)) = (text.is_empty(), self.m.find(&input.id)) {
                        let msg = self.m.requests.send_message(&self.m.paths, &mut self.m.sessions[i], &text);
                        self.ui.flash(msg, 8000.0);
                    }
                }
            }
            Key::Backspace => {
                if let Some(input) = self.ui.input.as_mut() {
                    input.text.pop();
                }
            }
            // Ctrl+U — стереть всё
            Key::CtrlU => {
                if let Some(input) = self.ui.input.as_mut() {
                    input.text.clear();
                }
            }
            Key::Char(c) if !c.is_control() => {
                if let Some(input) = self.ui.input.as_mut() {
                    input.text.push(c);
                }
            }
            _ => {}
        }
        false
    }

    /// Список сессий: выбор стрелками с кольцом, Enter — карточка.
    fn key_list(&mut self, up: bool, down: bool, open: bool) {
        let list = self.m.visible(self.ui.sel.as_deref());
        if list.is_empty() {
            return;
        }
        let n = list.len() as i64;
        let pos = list
            .iter()
            .position(|&x| Some(self.m.sessions[x].id.as_str()) == self.ui.sel.as_deref())
            .map_or(0, |p| p as i64);
        if up || down {
            let step = if up { -1 } else { 1 };
            let next = (pos + step + n) % n;
            self.ui.sel = Some(self.m.sessions[list[next as usize]].id.clone());
        } else if open {
            self.ui.view = View::Detail;
        }
    }

    /// Карточка сессии: Enter — весь план, s — советы, i — скиллы, b — идеи, m — сообщение, x — завершение.
    fn key_detail(&mut self, k: Key, open: bool, back: bool) {
        if open {
            self.ui.view = View::Plan;
            self.ui.plan_scroll = None;
        } else if ch(k, 's') || ch(k, 'ы') {
            self.ui.view = View::Tips;
            self.ui.tip_sel = 0;
        } else if ch(k, 'i') || ch(k, 'ш') {
            self.ui.view = View::Skills;
            self.ui.sk_scroll = 0;
        } else if ch(k, 'b') || ch(k, 'и') {
            self.ui.view = View::Ideas;
            self.ui.ideas_scroll = 0;
        } else if ch(k, 'm') || ch(k, 'ь') {
            match self.sel_index() {
                Some(i) if self.m.sessions[i].live => {
                    self.ui.input = Some(Input { id: self.m.sessions[i].id.clone(), text: String::new() });
                }
                _ => self.ui.flash("Сессия не запущена: сообщение некому передать.", 4000.0),
            }
        } else if ch(k, 'x') || ch(k, 'ч') {
            if let Some(i) = self.sel_index() {
                let s = &self.m.sessions[i];
                if s.reg.as_ref().is_some_and(|r| r.entrypoint == "claude-desktop") {
                    // десктопную сессию закрывает само приложение: ответ приходит сразу
                    let (tx, rx) = mpsc::channel();
                    procs::kill_session(
                        s,
                        Box::new(move |msg| {
                            let _ = tx.send(msg);
                        }),
                    );
                    if let Ok(msg) = rx.try_recv() {
                        self.ui.flash(msg, 8000.0);
                    }
                } else if s.live {
                    self.ui.confirm = Some(KillConfirm { id: s.id.clone(), until: clock::now() + 15000.0 });
                }
            }
        } else if back {
            self.ui.view = View::List;
        }
    }

    /// Советы: стрелки выбирают, Enter — отправить агенту или показать скрипт, u — скрипт, a — все сразу.
    fn key_tips(&mut self, k: Key, up: bool, down: bool, back: bool) {
        if up {
            self.ui.tip_sel -= 1;
        } else if down {
            self.ui.tip_sel += 1;
        } else if matches!(k, Key::Enter) || ch(k, 'u') || ch(k, 'г') {
            self.tip_action(k);
        } else if ch(k, 'a') || ch(k, 'ф') {
            if let Some(i) = self.sel_index() {
                let tips = self.m.active_tips(&self.m.sessions[i], clock::now()).active;
                let msg = self.m.requests.send_all(&self.m.paths, &mut self.m.sessions[i], &tips);
                self.ui.flash(msg, 6000.0);
            }
        } else if back {
            self.ui.view = View::Detail;
        }
    }

    /// Enter или u у выбранного совета: скрипт «для вас» или отправка агенту.
    fn tip_action(&mut self, k: Key) {
        let Some(i) = self.sel_index() else { return };
        let tips = self.m.active_tips(&self.m.sessions[i], clock::now()).active;
        let Some(t) = usize::try_from(self.ui.tip_sel).ok().and_then(|x| tips.get(x)) else { return };
        // u — скрипт у любого совета; Enter — скрипт только у советов, которые агенту не отправить
        let by_key = !matches!(k, Key::Enter);
        if let Some(fix) = t.fix.as_ref().filter(|_| by_key || !t.send) {
            match self.m.settings.plan(fix) {
                Some(plan) => {
                    self.ui.set_confirm = Some(SetConfirm {
                        id: self.m.sessions[i].id.clone(),
                        fix: fix.clone(),
                        plan,
                        until: clock::now() + 60000.0,
                    });
                }
                None => self.ui.flash("Настройка уже стоит, и ставил её не ccwatch: скрипту делать нечего.", 5000.0),
            }
        } else if !by_key {
            let msg = if t.send {
                self.m.requests.send_tip(&self.m.paths, &mut self.m.sessions[i], t)
            } else {
                "Этот совет — вручную: скрипта для него нет, что сделать, написано в описании.".to_string()
            };
            self.ui.flash(msg, 5000.0);
        }
    }

    /// История скиллов: стрелки и PgUp / PgDn прокручивают.
    fn key_skills(&mut self, k: Key, up: bool, down: bool, back: bool) {
        if up {
            self.ui.sk_scroll -= 1;
        } else if down {
            self.ui.sk_scroll += 1;
        } else if k == Key::PageUp {
            self.ui.sk_scroll -= 10;
        } else if k == Key::PageDown {
            self.ui.sk_scroll += 10;
        } else if back {
            self.ui.view = View::Detail;
        }
    }

    /// Идеи агента: Enter — спросить агента (выбранную сессию), стрелки и PgUp / PgDn прокручивают.
    fn key_ideas(&mut self, k: Key, up: bool, down: bool, back: bool) {
        if up {
            self.ui.ideas_scroll -= 1;
        } else if down {
            self.ui.ideas_scroll += 1;
        } else if k == Key::PageUp {
            self.ui.ideas_scroll -= 10;
        } else if k == Key::PageDown {
            self.ui.ideas_scroll += 10;
        } else if k == Key::Enter {
            if let Some(i) = self.sel_index() {
                let msg = self.m.requests.ask_ideas(&self.m.paths, &mut self.m.sessions[i]);
                self.ui.flash(msg, 6000.0);
            }
        } else if back {
            self.ui.view = View::Detail;
        }
    }

    /// Полный план: прокрутка стрелками, страницами, в начало и в конец.
    fn key_plan(&mut self, k: Key, up: bool, down: bool, back: bool) {
        let page = (self.ui.plan_page - 1).max(1);
        // None в прокрутке означает «0» в арифметике (как null в JS)
        let cur = self.ui.plan_scroll.unwrap_or(0);
        if up {
            self.ui.plan_scroll = Some(cur - 1);
        } else if down {
            self.ui.plan_scroll = Some(cur + 1);
        } else if k == Key::PageUp {
            self.ui.plan_scroll = Some(cur - page);
        } else if k == Key::PageDown {
            self.ui.plan_scroll = Some(cur + page);
        } else if k == Key::Home {
            self.ui.plan_scroll = Some(0);
        } else if k == Key::End {
            self.ui.plan_scroll = Some(1_000_000_000);
        } else if back {
            self.ui.view = View::Detail;
        }
    }
}
