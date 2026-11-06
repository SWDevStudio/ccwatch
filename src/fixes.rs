//! Скрипты советов «для вас»: совет, который агент выполнить не может, несёт скрипт — правку
//! ~/.claude/settings.json. Перед правкой файл копируется в settings.json.ccwatch-bak, а что меняли,
//! пишется в ~/.ccwatch/settings-log.json: повторный запуск на том же совете возвращает как было.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::ser::{PrettyFormatter, Serializer};
use serde_json::{json, Value};

use crate::clock;
use crate::json::{js_string, same};
use crate::paths::Paths;

/// Одна правка: по пути в settings.json поставить значение.
#[derive(Clone, Debug)]
pub struct Change {
    pub path: Vec<String>,
    pub to: Value,
    /// Человеческое описание правки: в шагах показываем его вместо «путь: было → стало» (для сложных правок).
    pub label: Option<String>,
}

/// Совет с скриптом: что делаем, на что повлияет и какие правки.
#[derive(Clone, Debug)]
pub struct Fix {
    pub what: String,
    pub note: String,
    pub changes: Vec<Change>,
}

/// Правка, которую выполнит скрипт. None — «не задано» (для `to` — удалить ключ).
#[derive(Clone, Debug)]
pub struct Planned {
    pub path: Vec<String>,
    pub from: Option<Value>,
    pub to: Option<Value>,
    /// описание для шагов, если у правки оно есть (у возврата — с пометкой «вернуть как было»)
    pub label: Option<String>,
}

/// Что сделает скрипт: поставить недостающее или вернуть прежнее (undo).
#[derive(Clone, Debug)]
pub struct FixPlan {
    pub undo: bool,
    pub changes: Vec<Planned>,
}

/// Запись журнала. Поле без значения в JSON не пишем — как undefined в JS.
#[derive(Serialize)]
struct LogEntry {
    at: i64,
    path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<Value>,
}

/// Файл settings.json и журнал. Читаем settings.json не чаще раза в 2 секунды:
/// экран советов перерисовывается часто, а перед правкой файл читается заново.
pub struct SettingsCache {
    settings: PathBuf,
    log: PathBuf,
    base: PathBuf,
    /// (когда читали, что прочитали — None: файла нет или он не разобран)
    cache: RefCell<(f64, Option<Value>)>,
}

impl SettingsCache {
    pub fn new(paths: &Paths) -> SettingsCache {
        SettingsCache {
            settings: paths.settings.clone(),
            log: paths.settings_log.clone(),
            base: paths.base.clone(),
            cache: RefCell::new((0.0, None)),
        }
    }

    /// settings.json как значение; fresh — читать заново, иначе — не чаще раза в 2 секунды.
    fn read(&self, fresh: bool) -> Option<Value> {
        let now = clock::now();
        let mut cache = self.cache.borrow_mut();
        if fresh || now - cache.0 > 2000.0 {
            *cache = (now, read_json(&self.settings));
        }
        cache.1.clone()
    }

    /// Совет уже выполнен: все нужные значения стоят на месте. Нет файла — настройки пустые.
    pub fn fix_done(&self, fix: &Fix) -> bool {
        let obj = self.read(false);
        fix.changes.iter().all(|c| get_path(obj.as_ref(), &c.path).is_some_and(|v| same(v, &c.to)))
    }

    /// Текущие хуки запуска (hooks.PostToolUse) из settings.json, с кэшем; нет файла или ключа — None.
    pub fn post_tool_use(&self) -> Option<Value> {
        self.read(false)?.get("hooks")?.get("PostToolUse").cloned()
    }

    /// Что сделает скрипт: поставить недостающие значения или, если всё уже стоит и ставил ccwatch, вернуть прежние.
    /// None — уже настроено не ccwatch, трогать нечего.
    pub fn plan(&self, fix: &Fix) -> Option<FixPlan> {
        let obj = self.read(true);
        let mut todo = Vec::new();
        for c in &fix.changes {
            let from = get_path(obj.as_ref(), &c.path).cloned();
            if !from.as_ref().is_some_and(|f| same(f, &c.to)) {
                todo.push(Planned { path: c.path.clone(), from, to: Some(c.to.clone()), label: c.label.clone() });
            }
        }
        if !todo.is_empty() {
            return Some(FixPlan { undo: false, changes: todo });
        }

        let log = self.read_log();
        let mut back = Vec::new();
        for c in &fix.changes {
            let key = c.path.join(".");
            let Some(last) = log.iter().filter(|e| entry_path(e) == key).last() else { continue };
            let (last_to, last_from) = (last.get("to"), last.get("from"));
            let to_matches = last_to.is_some_and(|t| same(t, &c.to));
            let from_differs = !last_from.is_some_and(|f| same(f, &c.to));
            if to_matches && from_differs {
                let label = c.label.as_ref().map(|l| format!("вернуть как было: {l}"));
                back.push(Planned { path: c.path.clone(), from: Some(c.to.clone()), to: last_from.cloned(), label });
            }
        }
        if back.is_empty() {
            None
        } else {
            Some(FixPlan { undo: true, changes: back })
        }
    }

    /// Выполняет скрипт: правит settings.json, делает копию и пишет журнал. Возвращает текст для показа.
    pub fn run(&self, fix: &Fix, plan: &FixPlan) -> String {
        // файла нет (например, новый ПК): начинаем с пустых настроек и копию делать не из чего
        let (mut obj, existed) = match fs::read(&self.settings) {
            Ok(bytes) => match parse_checked(&bytes) {
                Ok(v) => (v, true),
                Err(e) => return format!("Не удалось прочитать settings.json ({e}), поменяйте вручную."),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => (json!({}), false),
            Err(e) => return format!("Не удалось прочитать settings.json ({e}), поменяйте вручную."),
        };
        for c in &plan.changes {
            set_path(&mut obj, &c.path, c.to.as_ref());
        }
        if let Err(e) = self.save(&obj, plan, existed) {
            return format!("Не удалось записать settings.json: {e}");
        }
        self.cache.borrow_mut().0 = 0.0;
        let head = if plan.undo {
            format!("Вернул как было: {}.", fix.what)
        } else {
            format!("Готово: {}.", fix.what)
        };
        head + " Обычно Claude Code подхватывает правку на лету, иначе — после перезапуска сессии."
    }

    /// Копия (если был файл), запись через временный файл, затем журнал (последние 50 записей).
    fn save(&self, obj: &Value, plan: &FixPlan, existed: bool) -> io::Result<()> {
        if let Some(dir) = self.settings.parent() {
            fs::create_dir_all(dir)?;
        }
        if existed {
            fs::copy(&self.settings, with_suffix(&self.settings, ".ccwatch-bak"))?;
        }
        let tmp = with_suffix(&self.settings, ".ccwatch-tmp");
        let text = serde_json::to_string_pretty(obj).map_err(io::Error::other)? + "\n";
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.settings)?;

        fs::create_dir_all(&self.base)?;
        let at = clock::now() as i64;
        let mut log = self.read_log();
        for c in &plan.changes {
            let entry = LogEntry { at, path: c.path.clone(), from: c.from.clone(), to: c.to.clone() };
            log.push(serde_json::to_value(entry).map_err(io::Error::other)?);
        }
        let start = log.len().saturating_sub(50);
        fs::write(&self.log, to_indented(&log[start..]).map_err(io::Error::other)?)
    }

    /// Журнал изменений; нет файла или он не массив — пусто.
    fn read_log(&self) -> Vec<Value> {
        match read_json(&self.log) {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        }
    }
}

/// Значение по пути в объекте; нет пути или промежуточное значение не объект — None.
fn get_path<'a>(o: Option<&'a Value>, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(o?, |x, k| x.as_object()?.get(k))
}

/// Записывает значение по пути, недостающие промежуточные объекты создаёт пустыми.
/// to = None — удаляет ключ.
fn set_path(root: &mut Value, path: &[String], to: Option<&Value>) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut o = root;
    for k in parents {
        let Some(m) = o.as_object_mut() else { return };
        let next = m.entry(k.clone()).or_insert(Value::Null);
        if !next.is_object() {
            *next = json!({});
        }
        o = next;
    }
    let Some(m) = o.as_object_mut() else { return };
    match to {
        Some(v) => {
            m.insert(last.clone(), v.clone());
        }
        None => {
            m.shift_remove(last);
        }
    }
}

/// Путь записи журнала как строка через точку — для сравнения с путём правки.
fn entry_path(e: &Value) -> String {
    match e.get("path").and_then(Value::as_array) {
        Some(parts) => {
            parts.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(".")
        }
        None => String::new(),
    }
}

/// Шаги скрипта — их показываем перед подтверждением.
pub fn steps(plan: &FixPlan) -> Vec<String> {
    let mut out = vec!["сохранить копию ~/.claude/settings.json в settings.json.ccwatch-bak".to_string()];
    for c in &plan.changes {
        out.push(match &c.label {
            Some(label) => format!("в settings.json: {label}"),
            None => format!(
                "в settings.json: {}: {} → {}",
                c.path.join(" → "),
                show_val(c.from.as_ref()),
                show_val(c.to.as_ref())
            ),
        });
    }
    out.iter().enumerate().map(|(k, x)| format!("{}. {x}", k + 1)).collect()
}

/// Значение для показа: нет значения — «не задано», иначе как String() в JS.
fn show_val(v: Option<&Value>) -> String {
    v.map_or_else(|| "не задано".to_string(), js_string)
}

/// Файл как JSON-значение; нет файла или он не разобран — None.
fn read_json(file: &Path) -> Option<Value> {
    read_json_checked(file).ok()
}

/// Файл как JSON-значение, с текстом ошибки для показа.
fn read_json_checked(file: &Path) -> Result<Value, String> {
    let bytes = fs::read(file).map_err(|e| e.to_string())?;
    parse_checked(&bytes)
}

/// Байты файла как JSON-значение; файл с BOM в начале тоже разбираем (его пишет PowerShell).
pub fn parse_checked(bytes: &[u8]) -> Result<Value, String> {
    let text = String::from_utf8_lossy(bytes);
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
}

/// Путь с приписанным суффиксом: settings.json → settings.json.ccwatch-bak.
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// JSON с отступом в один пробел — так пишется журнал.
fn to_indented(entries: &[Value]) -> serde_json::Result<String> {
    let mut buf = Vec::new();
    let mut ser = Serializer::with_formatter(&mut buf, PrettyFormatter::with_indent(b" "));
    serde::Serialize::serialize(entries, &mut ser)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
