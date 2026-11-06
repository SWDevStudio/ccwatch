//! Советы о самой программе, а не о сессии: подключён ли хук ccwatch к Claude Code и на что он указывает.
//! Пока хук не на ccwatch.exe, совет «Хук ccwatch…» стоит первым в списке советов каждой сессии.
//! Его скрипт правит hooks.PostToolUse в settings.json: убирает хук ccwatch (старый Node-скрипт или прежний
//! ccwatch.exe) и ставит `ccwatch.exe hook`. Повторный запуск возвращает как было (журнал из fixes).

use std::env;
use std::path::Path;

use serde_json::{json, Value};

use crate::app::Monitor;
use crate::fixes::{Change, Fix};
use crate::paths::basename;
use crate::requests::{is_ccwatch_hook, HookKind};
use crate::session::Session;
use crate::tips::{ActiveTips, Kind, Tip};

impl Monitor {
    /// Советы о программе: сейчас только про хук. Пока хук на ccwatch.exe, советов нет.
    pub fn global_tips(&self) -> Vec<Tip> {
        let kind = self.requests.hook_kind;
        if kind == HookKind::Exe {
            return Vec::new();
        }
        let Ok(exe) = env::current_exe() else { return Vec::new() };
        let exe = exe_path(&exe);
        vec![hook_tip(kind, &exe, self.settings.post_tool_use().as_ref())]
    }

    /// Советы сессии, а перед ними — советы о программе: их видно и на экране советов, и в карточке.
    pub fn active_tips(&self, s: &Session, now: f64) -> ActiveTips {
        let mut at = s.active_tips(now);
        let mut head = self.global_tips();
        head.append(&mut at.active);
        at.active = head;
        at
    }
}

/// Путь к exe строкой; префикс \\?\, который Windows иногда добавляет, убираем.
fn exe_path(exe: &Path) -> String {
    let s = exe.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Совет «Хук ccwatch…»: для Node-хука — заменить на exe, для «не подключён» — добавить exe.
fn hook_tip(kind: HookKind, exe: &str, current: Option<&Value>) -> Tip {
    let name = basename(exe);
    let (title, detail, what, label) = match kind {
        HookKind::Node => (
            "Хук ccwatch работает через Node",
            "Запросы плана, советы и ваши сообщения доходят до сессий через хук. Сейчас он запускает Node \
             (ccwatch-hook.cmd → ccwatch-hook.js) — на ПК без Node он не заработает. Enter — скриптом переключить \
             хук на эту программу (ccwatch.exe hook): она делает то же самое и стартует быстрее.",
            "хук ccwatch на ccwatch.exe",
            format!("хук PostToolUse: ccwatch-hook.cmd (Node) → {name} hook"),
        ),
        _ => (
            "Хук ccwatch не подключён",
            "Без хука запросы плана, советы и ваши сообщения до сессий не доходят. Enter — скриптом подключить хук: \
             после каждого вызова инструмента Claude Code будет запускать ccwatch.exe hook — асинхронно, агента он \
             не задерживает, а без запросов сразу выходит.",
            "подключить хук ccwatch",
            format!("хук PostToolUse: + {name} hook"),
        ),
    };
    Tip {
        key: "hook".into(),
        kind: Kind::Setup,
        title: title.into(),
        cost: 0.0,
        unit: "",
        send: false,
        w: None,
        detail: detail.into(),
        text: String::new(),
        fix: Some(Fix {
            what: what.into(),
            note: format!("для всех сессий: хук запускает {exe} hook"),
            changes: vec![Change {
                path: vec!["hooks".into(), "PostToolUse".into()],
                to: hook_array(current, exe),
                label: Some(label),
            }],
        }),
    }
}

/// Новый hooks.PostToolUse: из текущего убираем хуки ccwatch (группы, ставшие пустыми, тоже), остальные
/// группы и их ключи (matcher и др.) не трогаем, в конец ставим `exe hook` асинхронно.
pub fn hook_array(current: Option<&Value>, exe: &str) -> Value {
    let mut out: Vec<Value> = Vec::new();
    for group in current.and_then(Value::as_array).into_iter().flatten() {
        let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
            out.push(group.clone());
            continue;
        };
        let rest: Vec<Value> = hooks.iter().filter(|h| !is_ccwatch_hook(h)).cloned().collect();
        if rest.len() == hooks.len() {
            out.push(group.clone());
        } else if !rest.is_empty() {
            let mut kept = group.clone();
            if let Some(m) = kept.as_object_mut() {
                m.insert("hooks".into(), Value::Array(rest));
            }
            out.push(kept);
        }
    }
    out.push(json!({"hooks": [{"type": "command", "command": exe, "args": ["hook"], "async": true}]}));
    Value::Array(out)
}
