//! Сессия Claude Code: её транскрипт, запись в реестре, план, субагенты и то, что из этого видно на экране.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use indexmap::IndexMap;
use serde_json::Value;

pub mod plan;
pub mod subagents;

pub use plan::{Estimate, Status, Step};
pub use subagents::{agent_time, Agent};

use subagents::Meta;

use crate::clock;
use crate::format::ago;
use crate::ideas::Idea;
use crate::json;
use crate::paths::{basename, Paths};
use crate::registry::Registration;
use crate::requests::{read_ask, Ask};
use crate::style::{c, SPIN};
use crate::text::take_chars;
use crate::tips::{Effect, Tip};
use crate::transcript::{Action, Block, Entry, Tokens, Transcript};

/// Откуда взят план: инструмент, строки ответа, файл-чек-лист.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PlanSource {
    None,
    Tool,
    Text,
    File,
}

/// Ответ агента на совет: когда, принял ли и что написал.
#[derive(Clone, Debug)]
pub struct Ack {
    pub t: f64,
    pub ok: bool,
    pub text: String,
}

pub struct Session {
    pub id: String,
    pub reg: Option<Registration>,
    pub live: bool,
    pub main: Option<Transcript>,
    /// субагенты, по порядку имён файлов
    pub agents: Vec<Agent>,
    pub plan: Vec<Step>,
    /// когда агент последний раз трогал план
    pub plan_at: f64,
    /// когда появился текущий план
    pub plan_born_at: f64,
    pub plan_source: PlanSource,
    pub plan_file_at: f64,
    /// ключ совета → ответ агента
    pub acks: HashMap<String, Ack>,
    /// идеи для анализатора: из строк ответа и из файла ответа, по времени
    pub ideas: Vec<Idea>,
    /// идеи из строк «ccwatch: идея …» в тексте ответа
    ideas_in_text: Vec<Idea>,
    /// идеи из файла ~/.ccwatch/ideas/<id>.md; файл перечитывается только при новом mtime
    ideas_in_file: Vec<Idea>,
    /// mtime файла ответа, который читали последним; 0 — файла ещё не было
    pub ideas_file_at: f64,
    /// когда агент ответил «идей нет» (в тексте или в файле); 0 — не отвечал
    pub ideas_none_at: f64,
    pub model: String,
    pub ctx: f64,
    pub title: String,
    /// последний инструмент главного агента
    pub last: Option<Action>,
    /// запрос плана: ждём ответа или уже отправлен
    pub ask: Option<Ask>,
    pub auto_asked: bool,
    pub subs_at: f64,
    /// tool_use id вызова TaskCreate → uid шага, ждущего номера задачи
    pending_tasks: HashMap<String, u64>,
    lookup_at: f64,
    /// meta.json субагентов по ключу файла
    metas: HashMap<String, Meta>,
    pub(crate) tips_cache: RefCell<Option<(f64, Vec<Tip>)>>,
    pub(crate) effects_cache: RefCell<Option<(f64, Vec<Effect>)>>,
}

/// Сводка плана для строки прогресса.
pub struct Progress {
    pub n: usize,
    pub done: usize,
    /// индекс шага в работе
    pub cur: Option<usize>,
    /// его номер с 1, или 0 если шага в работе нет
    pub idx: usize,
}

/// Значок и текст статуса сессии.
pub struct StatusView {
    pub icon: String,
    pub text: String,
    pub busy: bool,
}

/// Строка таблицы токенов: главный агент или группа субагентов одной модели.
pub struct Row {
    pub label: String,
    pub t: Tokens,
    /// число субагентов; у главного агента нет
    pub agents: Option<usize>,
    /// суммарное время субагентов в миллисекундах; у главного агента нет
    pub ms: Option<f64>,
}

/// Группа субагентов одной модели до превращения в строку.
struct SubRow {
    label: String,
    t: Tokens,
    agents: usize,
    ms: f64,
}

/// Короткое имя модели: без «claude-» и без даты в конце.
pub fn short_model(m: &str) -> String {
    let m = if m.is_empty() { "?" } else { m };
    let m = m.strip_prefix("claude-").unwrap_or(m);
    let b = m.as_bytes();
    let n = b.len();
    if n >= 9 && b[n - 9] == b'-' && b[n - 8..].iter().all(u8::is_ascii_digit) {
        m[..n - 9].to_string()
    } else {
        m.to_string()
    }
}

/// Имя точки входа для вывода: десктоп и cli.
pub fn entry_name(e: &str) -> String {
    match e {
        "claude-desktop" => "десктоп".into(),
        _ => e.into(),
    }
}

/// Время из строки транскрипта; неразбираемое или нулевое — текущее время.
fn when_of(at: &str) -> f64 {
    clock::parse_iso(at).filter(|t| *t != 0.0).unwrap_or_else(clock::now)
}

/// Поле как JS String(i[key]): нет поля — «undefined».
fn js_field(v: &Value, key: &str) -> String {
    match v.get(key) {
        None => "undefined".into(),
        Some(x) => json::js_string(x),
    }
}

/// Имя папки проекта: каждая единица UTF-16 вне a-zA-Z0-9 — «-».
fn project_dir(cwd: &str) -> String {
    let mut s = String::new();
    for ch in cwd.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
        } else {
            for _ in 0..ch.len_utf16() {
                s.push('-');
            }
        }
    }
    s
}

/// Транскрипт сессии: сначала по папке проекта из реестра, потом перебором всех папок.
pub fn find_transcript(paths: &Paths, id: &str, cwd: &str) -> Option<PathBuf> {
    if !cwd.is_empty() {
        let guess = paths.projects.join(project_dir(cwd)).join(format!("{id}.jsonl"));
        if guess.exists() {
            return Some(guess);
        }
    }
    let dirs = fs::read_dir(&paths.projects).ok()?;
    for d in dirs.flatten() {
        let f = d.path().join(format!("{id}.jsonl"));
        if f.exists() {
            return Some(f);
        }
    }
    None
}

/// Группа субагентов по модели; создаётся при первом упоминании, подпись задаётся тогда же.
fn sub_row<'a>(subs: &'a mut IndexMap<String, SubRow>, m: &str) -> &'a mut SubRow {
    subs.entry(m.to_string()).or_insert_with(|| SubRow {
        label: format!("агенты · {}", short_model(m)),
        t: Tokens::default(),
        agents: 0,
        ms: 0.0,
    })
}

impl Session {
    pub fn new(id: &str) -> Session {
        Session {
            id: id.to_string(),
            reg: None,
            live: false,
            main: None,
            agents: Vec::new(),
            plan: Vec::new(),
            plan_at: 0.0,
            plan_born_at: 0.0,
            plan_source: PlanSource::None,
            plan_file_at: 0.0,
            acks: HashMap::new(),
            ideas: Vec::new(),
            ideas_in_text: Vec::new(),
            ideas_in_file: Vec::new(),
            ideas_file_at: 0.0,
            ideas_none_at: 0.0,
            model: String::new(),
            ctx: 0.0,
            title: String::new(),
            last: None,
            ask: None,
            auto_asked: false,
            subs_at: 0.0,
            pending_tasks: HashMap::new(),
            lookup_at: 0.0,
            metas: HashMap::new(),
            tips_cache: RefCell::new(None),
            effects_cache: RefCell::new(None),
        }
    }

    /// Читает новые строки транскрипта, запрос плана и субагентов.
    pub fn refresh(&mut self, paths: &Paths, now: f64) {
        if self.main.is_none() && now - self.lookup_at > 5000.0 {
            self.lookup_at = now;
            let cwd = self.reg.as_ref().map(|r| r.cwd.clone()).unwrap_or_default();
            if let Some(f) = find_transcript(paths, &self.id, &cwd) {
                self.main = Some(Transcript::new(f));
            }
        }
        self.ask = read_ask(paths, &self.id);
        self.read_ideas_file(paths);
        // транскрипт вынимаем из сессии, чтобы разбор строк мог менять саму сессию
        let Some(mut main) = self.main.take() else { return };
        main.pull(true, &mut |d: &Entry| self.on_line(d));
        self.main = Some(main);
        self.read_plan_file(paths);
        if now - self.subs_at > 2000.0 {
            self.subs_at = now;
            self.pull_subagents();
        }
    }

    /// Одна строка транскрипта: название, модель, контекст, план из ответа и вызовы инструментов.
    fn on_line(&mut self, d: &Entry) {
        if d.kind == "ai-title" {
            if !d.ai_title.is_empty() {
                self.title = d.ai_title.clone();
            }
            return;
        }
        if d.is_sidechain {
            return;
        }
        if d.kind == "user" {
            self.on_task_created(d);
            return;
        }
        if d.kind != "assistant" {
            return;
        }
        let Some(m) = &d.message else { return };
        if !m.model.is_empty() && !m.model.starts_with('<') {
            self.model = m.model.clone();
        }
        if let Some(u) = &m.usage {
            let ctx = u.context();
            if ctx != 0.0 {
                self.ctx = ctx;
            }
        }
        for b in &m.content {
            if b.kind == "text" {
                if let Some(text) = b.text.as_str() {
                    if text.contains("ccwatch:") {
                        self.on_text(text, &d.timestamp);
                    }
                }
            }
        }
        for b in d.tool_uses() {
            self.on_tool(b, &d.timestamp);
        }
    }

    /// Вызов инструмента: меняет план. Инструменты самого плана (update_plan, wrap_up)
    /// не становятся «последним действием» — как в JS, где для них выход раньше.
    fn on_tool(&mut self, b: &Block, at: &str) {
        let i = &b.input;
        let when = when_of(at);
        let prev = self.plan.clone();
        match b.name.as_str() {
            "mcp__ccd_turn__update_plan" => {
                self.plan = plan::apply_plan(std::mem::take(&mut self.plan), i);
                self.plan_changed(prev, when, PlanSource::Tool);
                return;
            }
            "mcp__ccd_turn__wrap_up" => {
                for s in self.plan.iter_mut() {
                    s.status = Status::Completed;
                }
                let source = self.plan_source;
                self.plan_changed(prev, when, source);
                return;
            }
            "TodoWrite" => {
                if let Some(todos) = i.get("todos").and_then(Value::as_array) {
                    self.plan =
                        todos.iter().map(|t| plan::step(&json::text(t, "content"), &json::text(t, "status"))).collect();
                }
                self.plan_changed(prev, when, PlanSource::Tool);
            }
            "TaskCreate" => {
                let s = plan::step(&json::first_truthy(i, &["subject", "description"]), "pending");
                // номер задачи придёт в ответе инструмента — запоминаем шаг до тех пор
                self.pending_tasks.insert(b.id.clone(), s.uid);
                self.plan.push(s);
                self.plan_changed(prev, when, PlanSource::Tool);
            }
            "TaskUpdate" => {
                let key = js_field(i, "taskId");
                if let Some(pos) = self.plan.iter().position(|x| x.id.as_deref() == Some(key.as_str())) {
                    if json::text(i, "status") == "deleted" {
                        self.plan.remove(pos);
                    } else {
                        let s = &mut self.plan[pos];
                        let status = json::text(i, "status");
                        if !status.is_empty() {
                            s.status = Status::from_str(&status);
                        }
                        if json::has(i, "subject") {
                            s.content = plan::step(&json::text(i, "subject"), "pending").content;
                        }
                    }
                }
                self.plan_changed(prev, when, PlanSource::Tool);
            }
            _ => {}
        }
        self.last = Some(Action::of(b, at));
    }

    /// Строка tool_result для TaskCreate: номер задачи приходит в ответе как «Task #3 created».
    fn on_task_created(&mut self, d: &Entry) {
        let Some(task) = d.tool_use_result.get("task").filter(|t| json::truthy(t)) else { return };
        let Some(res) = d.message.as_ref().and_then(|m| m.content.iter().find(|b| b.kind == "tool_result")) else {
            return;
        };
        let Some(uid) = self.pending_tasks.get(&res.tool_use_id).copied() else { return };
        if let Some(s) = self.plan.iter_mut().find(|x| x.uid == uid) {
            s.id = Some(js_field(task, "id"));
        }
        self.pending_tasks.remove(&res.tool_use_id);
    }

    /// Имя для списка: из реестра, заголовка транскрипта, папки или первые 8 символов id.
    pub fn name(&self) -> String {
        let reg_name = self.reg.as_ref().map(|r| r.name.as_str()).unwrap_or("");
        if !reg_name.is_empty() {
            return reg_name.to_string();
        }
        if !self.title.is_empty() {
            return self.title.clone();
        }
        let cwd = self.reg.as_ref().map(|r| r.cwd.as_str()).unwrap_or("");
        let dir = basename(cwd);
        if !dir.is_empty() {
            return dir;
        }
        take_chars(&self.id, 8)
    }

    /// Сколько шагов, сколько готово и какой в работе.
    pub fn progress(&self) -> Progress {
        let n = self.plan.len();
        let done = self.plan.iter().filter(|x| x.status == Status::Completed).count();
        let cur = self.plan.iter().position(|x| x.status == Status::InProgress);
        Progress { n, done, cur, idx: cur.map_or(0, |k| k + 1) }
    }

    /// Значок и текст статуса: работает, готово, ждёт вас или завершена.
    pub fn status(&self, now: f64) -> StatusView {
        if !self.live {
            return StatusView { icon: c::dim("○"), text: c::dim("завершена"), busy: false };
        }
        let default = Registration::default();
        let r = self.reg.as_ref().unwrap_or(&default);
        let stamp = [r.status_updated_at, r.updated_at].into_iter().find(|t| *t != 0.0).unwrap_or(now);
        let since = ago(now - stamp);
        if r.status == "busy" {
            let frame = ((now / 100.0).floor() as i64).rem_euclid(SPIN.len() as i64) as usize;
            return StatusView {
                icon: c::pink(SPIN[frame].to_string()),
                text: c::pink(format!("работает {since}")),
                busy: true,
            };
        }
        let p = self.progress();
        if r.status == "idle" && p.n > 0 && p.done == p.n {
            return StatusView {
                icon: c::green("✓"), text: c::green(format!("готово {since} назад")), busy: false
            };
        }
        if r.status == "idle" {
            return StatusView {
                icon: c::amber("●"), text: c::amber(format!("ждёт вас {since}")), busy: false
            };
        }
        let other = if r.status.is_empty() { "?" } else { r.status.as_str() };
        StatusView { icon: c::amber("●"), text: c::amber(other), busy: false }
    }

    /// Субагенты, которые сейчас работают.
    pub fn running(&self) -> impl Iterator<Item = &Agent> {
        self.agents.iter().filter(|a| a.running)
    }

    /// Самое свежее действие: главного агента или одного из работающих субагентов.
    /// Второе значение — имя субагента, пустая строка для главного агента.
    pub fn activity(&self) -> Option<(Action, String)> {
        let mut best: Option<(Action, String)> = self.last.clone().map(|l| (l, String::new()));
        for a in self.running() {
            let Some(last) = &a.t.last else { continue };
            let newer = match &best {
                None => true,
                Some((b, _)) => last.at > b.at,
            };
            if newer {
                best = Some((last.clone(), a.name.clone()));
            }
        }
        best
    }

    /// Токены сессии: главный агент и все субагенты.
    pub fn tokens(&self) -> Tokens {
        let mut t = self.main.as_ref().map(|m| m.totals()).unwrap_or_default();
        for a in &self.agents {
            t.add(&a.t.totals());
        }
        t
    }

    /// Строки токенов по моделям: сначала главный агент, потом субагенты по убыванию выходных токенов.
    pub fn rows(&self, now: f64) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(main) = &self.main {
            for (m, b) in &main.by_model {
                rows.push(Row {
                    label: format!("главный · {}", short_model(m)), t: *b, agents: None, ms: None
                });
            }
        }
        let mut subs: IndexMap<String, SubRow> = IndexMap::new();
        for a in &self.agents {
            for (m, b) in &a.t.by_model {
                sub_row(&mut subs, m).t.add(b);
            }
            let model = if a.t.model.is_empty() { a.model.as_str() } else { a.t.model.as_str() };
            let r = sub_row(&mut subs, model);
            r.agents += 1;
            r.ms += agent_time(a, now);
        }
        let mut sorted: Vec<SubRow> = subs.into_values().collect();
        sorted.sort_by(|a, b| b.t.o.partial_cmp(&a.t.o).unwrap_or(Ordering::Equal));
        rows.extend(sorted.into_iter().map(|s| Row { label: s.label, t: s.t, agents: Some(s.agents), ms: Some(s.ms) }));
        rows
    }
}
