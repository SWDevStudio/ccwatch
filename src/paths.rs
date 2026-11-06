//! Где лежат файлы: данные Claude Code и рабочая папка ccwatch.

use std::env;
use std::path::{Path, PathBuf};

pub struct Paths {
    /// ~/.claude (или CLAUDE_CONFIG_DIR)
    pub root: PathBuf,
    /// реестр живых сессий: <pid>.json
    pub sessions: PathBuf,
    /// транскрипты: <проект>/<id>.jsonl
    pub projects: PathBuf,
    /// настройки Claude Code — их правят скрипты советов «для вас»
    pub settings: PathBuf,
    /// ~/.ccwatch (или CCWATCH_DIR)
    pub base: PathBuf,
    /// запросы к сессиям, ждущие доставки хуком
    pub ask: PathBuf,
    /// отметки хука о доставке
    pub done: PathBuf,
    /// чек-листы по первой версии запроса: сессии, получившие её, ведут план там
    pub plans: PathBuf,
    /// что меняли скрипты советов — по нему повторный запуск возвращает как было
    pub settings_log: PathBuf,
    /// копилка идей агента для анализатора по всем сессиям: по строке JSON на идею
    pub ideas: PathBuf,
    /// ответы агентов на вопрос об идеях: <id сессии>.md, пишет агент инструментом
    pub ideas_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Paths {
        let home = home_dir();
        let root = env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
        let base = env_dir("CCWATCH_DIR").unwrap_or_else(|| home.join(".ccwatch"));
        Paths::new(&root, &base)
    }

    pub fn new(root: &Path, base: &Path) -> Paths {
        Paths {
            root: root.to_path_buf(),
            sessions: root.join("sessions"),
            projects: root.join("projects"),
            settings: root.join("settings.json"),
            base: base.to_path_buf(),
            ask: base.join("ask"),
            done: base.join("done"),
            plans: base.join("plans"),
            settings_log: base.join("settings-log.json"),
            ideas: base.join("ideas.jsonl"),
            ideas_dir: base.join("ideas"),
        }
    }
}

/// Пустая переменная окружения — как её отсутствие (в Node это `||`).
fn env_dir(name: &str) -> Option<PathBuf> {
    env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn home_dir() -> PathBuf {
    env_dir("USERPROFILE").or_else(|| env_dir("HOME")).unwrap_or_else(|| PathBuf::from("."))
}

/// Имя файла из пути с любыми разделителями — как path.basename в Node на Windows.
pub fn basename(p: &str) -> String {
    let p = p.trim_end_matches(['/', '\\']);
    p.rsplit(['/', '\\']).next().unwrap_or("").to_string()
}
