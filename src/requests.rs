//! Запросы к сессии через хук (`ccwatch hook`): просьба о плане, советы для агента, ваше сообщение.
//! Монитор кладёт запрос в ~/.ccwatch/ask/<id>.json, хук в сессии забирает его при следующем действии
//! агента и пишет отметку о доставке в ~/.ccwatch/done/<id>.json.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use serde_json::{json, Value};

use crate::clock;
use crate::format::{ago, base36};
use crate::json as j;
use crate::paths::Paths;
use crate::session::Session;
use crate::style::c;
use crate::text::clip;
use crate::tips::Tip;

/// Ключ служебного вопроса «какие идеи для анализатора» (в тексте и в метке доставки ccwatch:ideas).
pub const IDEAS_KEY: &str = "ideas";

/// Вопрос агенту: какие правила он добавил бы в анализатор. Ответ агент пишет инструментом в файл `path`:
/// текст ответа в десктопе переписывает оформитель, а вызов инструмента — нет.
pub fn ideas_text(path: &str) -> String {
    format!(
        "Вопрос от монитора ccwatch (ccwatch:ideas), пользователь просит ответить. ccwatch — статический анализатор логов Claude Code: по транскриптам он находит, где агенты теряют время и токены (долгие и повторные проверки, большие выводы в контексте, разросшиеся субагенты), и советует, как этого избежать. Вспомни свою работу в этой сессии: что отнимало заметно больше времени или токенов, чем нужно, повторялось и видно по логам? Например: тесты на TypeScript гоняются без кэша компиляции, и каждый прогон идёт минутами. Предложи до трёх правил для анализатора и запиши их одним вызовом инструмента в файл {path} — по строке на правило: «<короткое название> — <как заметить по логу: инструменты, команды, длительности, повторы> — <что посоветовать>». Если предложить нечего — запиши в файл одну строку «идей нет». Отвечать в тексте сообщения не нужно: оформление может его переписать. Текущую работу не прерывай."
    )
}

// Запрос к сессии: {at, from, plan — просьба о плане, autoPlan — её ccwatch попросил сам,
// keys / texts — выбранные советы, ttl — сколько ждать доставки}. В старых запросах полей нет — это просьба о плане.

/// Автоматическая просьба о плане нужна, только пока открыт монитор.
pub const AUTO_TTL: f64 = 10.0 * 60e3;
/// То, что вы отправили сами, ждёт, пока сессия снова начнёт работать.
pub const MANUAL_TTL: f64 = 12.0 * 3600e3;

/// Подсказка, если хук не подключён к Claude Code: запрос до сессии не дойдёт.
const NO_HOOK: &str = " Но хук ccwatch не подключён: до сессии запрос не дойдёт.";
/// То же для советов, которые отправлены пачкой.
const NO_HOOK_TIPS: &str = " Но хук ccwatch не подключён: до сессии они не дойдут.";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AskState {
    /// запрос лежит в ask/ и ждёт доставки
    Wait,
    /// хук уже передал его сессии (done/)
    Sent,
}

#[derive(Clone, Debug)]
pub struct Ask {
    pub state: AskState,
    pub at: f64,
    pub plan: bool,
    pub texts: Vec<String>,
    pub keys: Vec<String>,
    /// срок ожидания; у отправленного запроса его нет
    pub ttl: Option<f64>,
}

/// Что добавить в запрос: просьбу о плане или совет (или ваше сообщение — тоже совет с ключом msg…).
pub enum Add {
    Plan { auto: bool },
    Text { key: String, text: String },
}

/// Ваше сообщение сессии за этот запуск монитора.
#[derive(Clone, Debug)]
struct SentMsg {
    key: String,
    text: String,
    at: f64,
}

/// Какой хук ccwatch подключён в settings.json.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HookKind {
    /// хука ccwatch нет
    None,
    /// старый хук: ccwatch-hook.cmd / ccwatch-hook.js, нужен Node
    Node,
    /// ccwatch.exe hook: без Node и компиляторов
    Exe,
}

pub struct Requests {
    /// подключён ли хук к settings.json (проверка раз в 5 секунд)
    pub hook_installed: bool,
    /// какой именно хук подключён: от этого зависит совет «Хук ccwatch…»
    pub hook_kind: HookKind,
    hook_checked_at: f64,
    /// автоматические просьбы о плане этого запуска: при выходе снимаем
    auto_asks: BTreeSet<String>,
    /// id сессии → ваши сообщения ей
    sent: HashMap<String, Vec<SentMsg>>,
}

impl Requests {
    pub fn new() -> Requests {
        Requests {
            hook_installed: false,
            hook_kind: HookKind::None,
            hook_checked_at: 0.0,
            auto_asks: BTreeSet::new(),
            sent: HashMap::new(),
        }
    }

    /// Подключён ли хук. Кэш на 5 секунд; при now = 0 всегда отдаёт сохранённое значение.
    pub fn check_hook(&mut self, paths: &Paths, now: f64) -> bool {
        if now - self.hook_checked_at < 5000.0 {
            return self.hook_installed;
        }
        self.hook_checked_at = now;
        self.hook_kind = fs::read(&paths.settings)
            .ok()
            .and_then(|b| crate::fixes::parse_checked(&b).ok())
            .map_or(HookKind::None, |v| hook_kind_in(&v));
        self.hook_installed = self.hook_kind != HookKind::None;
        self.hook_installed
    }

    /// Пишет к ещё не доставленному запросу сессии просьбу о плане или совет. Пустая строка — всё хорошо,
    /// иначе текст для показа (ошибка или предупреждение про хук).
    pub fn queue(&mut self, paths: &Paths, s: Option<&mut Session>, add: Add) -> String {
        let s = match s {
            Some(s) if s.live => s,
            _ => return "Сессия не запущена.".to_string(),
        };
        if let Err(e) = self.write_ask(paths, s, add) {
            return format!("Не удалось записать запрос: {e}");
        }
        if self.hook_installed {
            String::new()
        } else {
            NO_HOOK.to_string()
        }
    }

    /// Просьба сессии написать план.
    pub fn ask_plan(&mut self, paths: &Paths, s: Option<&mut Session>, auto: bool) -> String {
        let err = self.queue(paths, s, Add::Plan { auto });
        if err.starts_with(' ') || err.is_empty() {
            format!("План запрошен.{err}")
        } else {
            err
        }
    }

    /// Один совет для агента. Советы «для вас» агенту не уходят.
    pub fn send_tip(&mut self, paths: &Paths, s: &mut Session, tip: &Tip) -> String {
        if !tip.send {
            return "Этот совет для вас: агент тут ничего изменить не может.".to_string();
        }
        let err = self.queue(paths, Some(&mut *s), Add::Text { key: tip.key.clone(), text: tip.text.clone() });
        if !err.is_empty() && !err.starts_with(' ') {
            return err;
        }
        let idle = if s.reg.as_ref().is_some_and(|r| r.status != "busy") {
            " Сессия сейчас ждёт вас: совет дойдёт, когда она снова начнёт работать (ждёт до 12 часов)."
        } else {
            ""
        };
        format!("Совет отправлен, дойдёт при следующем действии агента.{idle}{err}")
    }

    /// Все советы для агента одним сообщением.
    pub fn send_all(&mut self, paths: &Paths, s: &mut Session, tips: &[Tip]) -> String {
        let ok: Vec<&Tip> = tips.iter().filter(|t| t.send).collect();
        if ok.is_empty() {
            return "Советов для агента нет: оставшиеся — для вас.".to_string();
        }
        for t in &ok {
            let msg = self.send_tip(paths, s, t);
            if !msg.starts_with("Совет отправлен") {
                return msg;
            }
        }
        let hook_note = if self.hook_installed { "" } else { NO_HOOK_TIPS };
        format!("Отправлено советов: {}, дойдут одним сообщением при следующем действии агента.{hook_note}", ok.len())
    }

    /// Вопрос агенту об идеях для анализатора (экран идей, Enter). Ответ придёт строками «ccwatch: идея …».
    pub fn ask_ideas(&mut self, paths: &Paths, s: &mut Session) -> String {
        // папка для ответов: агент создаст файл сам, но папку можно завести заранее
        let _ = fs::create_dir_all(&paths.ideas_dir);
        let file = paths.ideas_dir.join(format!("{}.md", s.id)).display().to_string();
        let add = Add::Text { key: IDEAS_KEY.to_string(), text: ideas_text(&file) };
        let err = self.queue(paths, Some(&mut *s), add);
        if !err.is_empty() && !err.starts_with(' ') {
            return err;
        }
        let idle = if s.reg.as_ref().is_some_and(|r| r.status != "busy") {
            " Сессия сейчас ждёт вас: вопрос дойдёт, когда она снова начнёт работать (ждёт до 12 часов)."
        } else {
            ""
        };
        format!("Вопрос отправлен, агент ответит в своём следующем сообщении.{idle}{err}")
    }

    /// Ваше сообщение сессии (клавиша m): тот же хук, метка ccwatch:msg… показывает, когда оно дошло.
    pub fn send_message(&mut self, paths: &Paths, s: &mut Session, text: &str) -> String {
        let now = clock::now();
        let key = format!("msg{}", base36(now as u64));
        let body = format!(
            "Пользователь написал тебе из монитора ccwatch (ccwatch:{key}): «{text}». Отнесись к этому как к его сообщению в чате."
        );
        let err = self.queue(paths, Some(&mut *s), Add::Text { key: key.clone(), text: body });
        if !err.is_empty() && !err.starts_with(' ') {
            return err;
        }
        self.sent.entry(s.id.clone()).or_default().push(SentMsg { key, text: text.to_string(), at: now });
        let idle = if s.reg.as_ref().is_some_and(|r| r.status != "busy") {
            " Сессия сейчас ждёт вас: сообщение дойдёт, когда она снова начнёт работать."
        } else {
            ""
        };
        format!("Сообщение отправлено, дойдёт при следующем действии агента.{idle}{err}")
    }

    /// Последнее ваше сообщение этой сессии и дошло ли оно.
    pub fn msg_line(&self, s: &Session, now: f64) -> Option<String> {
        let m = self.sent.get(&s.id)?.last()?;
        let got = s.main.as_ref().and_then(|t| t.deliveries.iter().find(|d| d.key == m.key));
        let text = format!("Ваше сообщение «{}» ", clip(&m.text, 50));
        Some(match got {
            Some(d) => c::green(format!("{text}дошло {} назад.", ago(now - d.t))),
            None => c::amber(format!("{text}ждёт доставки ({}).", ago(now - m.at))),
        })
    }

    /// Строка о запросе плана под сессией.
    pub fn ask_line(&self, s: &Session, now: f64) -> Option<String> {
        let a = s.ask.as_ref()?;
        if !a.plan {
            return None;
        }
        if a.state == AskState::Wait {
            let line =
                c::amber(format!("План запрошен {} назад, дойдёт при следующем действии агента.", ago(now - a.at)));
            let hook = if self.hook_installed { String::new() } else { c::pink(" Хук не подключён!") };
            return Some(line + &hook);
        }
        // план уже пришёл после запроса или запрос старше 10 минут — строку не показываем
        if s.plan_at > a.at || now - a.at > 10.0 * 60e3 {
            return None;
        }
        Some(c::green(format!("Запрос передан {} назад, ждём список шагов.", ago(now - a.at))))
    }

    /// Снимает автоматические просьбы о плане при выходе из монитора. Ручные советы и просьбы остаются.
    pub fn drop_my_asks(&mut self, paths: &Paths) {
        for id in std::mem::take(&mut self.auto_asks) {
            let f = paths.ask.join(format!("{id}.json"));
            let Some(r) = read_object(&f) else { continue };
            if !j::has(&r, "autoPlan") {
                continue;
            }
            if has_texts(&r) {
                let mut next = r.clone();
                if let Some(m) = next.as_object_mut() {
                    m.insert("plan".into(), json!(false));
                    m.insert("autoPlan".into(), json!(false));
                    m.insert("ttl".into(), json!(MANUAL_TTL as i64));
                }
                if let Ok(text) = serde_json::to_string(&next) {
                    let _ = fs::write(&f, text);
                }
            } else {
                let _ = fs::remove_file(&f);
            }
        }
    }

    /// Дописывает запрос в ask/<id>.json и обновляет запрос сессии.
    fn write_ask(&mut self, paths: &Paths, s: &mut Session, add: Add) -> io::Result<()> {
        fs::create_dir_all(&paths.ask)?;
        let file = paths.ask.join(format!("{}.json", s.id));
        let cur = Cur::read(&file);

        // просьба о плане остаётся, даже если её добавили без автоматики; автоматической она становится только при своём запросе
        let (plan, auto) = match &add {
            Add::Plan { auto } => (true, *auto && (!cur.plan || cur.auto)),
            Add::Text { .. } => (cur.plan, cur.plan && cur.auto),
        };

        let mut texts = cur.texts;
        let mut keys = cur.keys;
        if let Add::Text { key, text } = &add {
            if !text.is_empty() {
                // тот же совет заменяет прежний текст, новый — дописывается
                match keys.iter().position(|k| k.as_str() == Some(key.as_str())) {
                    Some(i) => {
                        if texts.len() <= i {
                            texts.resize(i + 1, Value::Null);
                        }
                        texts[i] = json!(text);
                    }
                    None => {
                        texts.push(json!(text));
                        keys.push(json!(key));
                    }
                }
            }
        }

        // без советов автоматическая просьба живёт 10 минут, с советами — как ручная
        let ttl = if auto && texts.is_empty() { AUTO_TTL } else { MANUAL_TTL };
        let body = json!({
            "at": clock::now() as i64,
            "from": "ccwatch",
            "plan": plan,
            "autoPlan": auto,
            "keys": keys,
            "texts": texts,
            "ttl": ttl as i64,
        });
        fs::write(&file, serde_json::to_string(&body).map_err(io::Error::other)?)?;
        let _ = fs::remove_file(paths.done.join(format!("{}.json", s.id)));
        if auto {
            self.auto_asks.insert(s.id.clone());
        }
        s.ask = read_ask(paths, &s.id);
        Ok(())
    }
}

/// Текущий запрос из файла (или пустой, если файла нет или он не разобран).
struct Cur {
    plan: bool,
    auto: bool,
    texts: Vec<Value>,
    keys: Vec<Value>,
}

impl Cur {
    fn read(file: &Path) -> Cur {
        match read_object(file) {
            Some(r) => Cur {
                plan: is_plan_req(&r),
                auto: j::has(&r, "autoPlan"),
                texts: array(&r, "texts"),
                keys: array(&r, "keys"),
            },
            None => Cur { plan: false, auto: false, texts: Vec::new(), keys: Vec::new() },
        }
    }
}

/// Запрос из файла, если это JSON-объект. Битый файл или не объект — None.
fn read_object(file: &Path) -> Option<Value> {
    let bytes = fs::read(file).ok()?;
    match serde_json::from_str::<Value>(&String::from_utf8_lossy(&bytes)).ok()? {
        v @ Value::Object(_) => Some(v),
        _ => None,
    }
}

/// Поле-массив как есть; нет поля или не массив — пустой список.
fn array(r: &Value, key: &str) -> Vec<Value> {
    r.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// Поле-массив строк (в JS `r.texts || []`).
fn strings(r: &Value, key: &str) -> Vec<String> {
    array(r, key).iter().map(j::js_string).collect()
}

fn has_texts(r: &Value) -> bool {
    r.get("texts").and_then(Value::as_array).is_some_and(|a| !a.is_empty())
}

/// Просьба о плане: без поля plan=false, и не совет без поля plan (старые запросы — только просьба о плане).
fn is_plan_req(r: &Value) -> bool {
    let plan = r.get("plan");
    let not_false = !matches!(plan, Some(Value::Bool(false)));
    !(has_texts(r) && plan.is_none()) && not_false
}

/// Запрос сессии: сначала ожидающий доставки (ask/), иначе отметка о доставке (done/).
pub fn read_ask(paths: &Paths, id: &str) -> Option<Ask> {
    read_waiting(paths, id).or_else(|| read_delivered(paths, id))
}

fn read_waiting(paths: &Paths, id: &str) -> Option<Ask> {
    let f = paths.ask.join(format!("{id}.json"));
    let r = read_object(&f)?;
    let at = clock::mtime_ms(&fs::metadata(&f).ok()?);
    let ttl = if j::has(&r, "ttl") { j::number(&r, "ttl") } else { AUTO_TTL };
    Some(Ask {
        state: AskState::Wait,
        at,
        plan: is_plan_req(&r),
        texts: strings(&r, "texts"),
        keys: strings(&r, "keys"),
        ttl: Some(ttl),
    })
}

fn read_delivered(paths: &Paths, id: &str) -> Option<Ask> {
    let r = read_object(&paths.done.join(format!("{id}.json")))?;
    Some(Ask {
        state: AskState::Sent,
        at: j::number(&r, "deliveredAt"),
        plan: is_plan_req(&r),
        texts: strings(&r, "texts"),
        keys: strings(&r, "keys"),
        ttl: None,
    })
}

/// Убирает просроченные запросы (у каждого свой срок) и отметки о доставке старше суток.
pub fn clean_asks(paths: &Paths) {
    let now = clock::now();
    for f in files_in(&paths.ask) {
        let Some(mtime) = fs::metadata(&f).ok().map(|m| clock::mtime_ms(&m)) else { continue };
        let ttl = read_json(&f).map(|r| if j::has(&r, "ttl") { j::number(&r, "ttl") } else { AUTO_TTL });
        if now - mtime > ttl.unwrap_or(AUTO_TTL) {
            let _ = fs::remove_file(&f);
        }
    }
    for f in files_in(&paths.done) {
        let Some(mtime) = fs::metadata(&f).ok().map(|m| clock::mtime_ms(&m)) else { continue };
        if now - mtime > 24.0 * 3600e3 {
            let _ = fs::remove_file(&f);
        }
    }
}

/// Файлы папки; нет папки — пусто.
fn files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default()
}

/// Любой JSON из файла, без проверки типа.
fn read_json(file: &Path) -> Option<Value> {
    let bytes = fs::read(file).ok()?;
    serde_json::from_str(&String::from_utf8_lossy(&bytes)).ok()
}

/// Какой хук ccwatch подключён: смотрим hooks.PostToolUse[*].hooks[*]. Хук ccwatch — тот, где в команде
/// или аргументах есть «ccwatch». Старый Node-хук главнее: если он есть, остальное не важно, его и переключаем.
pub fn hook_kind_in(settings: &Value) -> HookKind {
    let groups = settings.get("hooks").and_then(|h| h.get("PostToolUse")).and_then(Value::as_array);
    let hooks = groups.into_iter().flatten().filter_map(|g| g.get("hooks").and_then(Value::as_array)).flatten();
    let mut kind = HookKind::None;
    for hook in hooks {
        let text = hook_text(hook);
        if !text.contains("ccwatch") {
            continue;
        }
        if text.contains("ccwatch-hook") {
            return HookKind::Node;
        }
        kind = HookKind::Exe;
    }
    kind
}

/// Хук ccwatch (старый или новый): в команде или аргументах есть «ccwatch» без учёта регистра.
pub fn is_ccwatch_hook(hook: &Value) -> bool {
    hook_text(hook).contains("ccwatch")
}

/// Команда и аргументы хука одной строкой в нижнем регистре — по ней ищем ccwatch.
fn hook_text(hook: &Value) -> String {
    let mut text = j::text(hook, "command");
    for a in hook.get("args").and_then(Value::as_array).into_iter().flatten() {
        text.push(' ');
        text.push_str(&j::js_string(a));
    }
    text.to_lowercase()
}
