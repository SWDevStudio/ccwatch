//! Хук ccwatch для Claude Code: подкоманда `ccwatch hook`. Пока монитор открыт, он кладёт запрос
//! «покажи план» (и выбранные пользователем советы по экономии токенов) в ~/.ccwatch/ask/<sessionId>.json.
//! Хук на ближайшем вызове инструмента этой сессии передаёт запрос агенту через additionalContext
//! и отмечает доставку в ~/.ccwatch/done/. Агент без инструмента списка задач пишет план текстом ответа,
//! дальше ставит короткие строки-метки, ccwatch читает их из транскрипта.
//!
//! Подключение: монитор сам предлагает совет «Хук ccwatch…» (список советов, клавиша s в карточке сессии),
//! либо вручную в ~/.claude/settings.json (Windows; путь к своему ccwatch.exe):
//!   "hooks": { "PostToolUse": [{ "hooks": [{ "type": "command", "command": "C:\\...\\ccwatch.exe",
//!     "args": ["hook"], "async": true }] }] }
//! async не даёт хуку задерживать агента. Нет ни одного запроса в ask/ — выход сразу, stdin не читаем.
//! Прежний хук был Node-скриптом ccwatch-hook.js: совет переключает его на ccwatch.exe.
//! Ошибки молча дают код 0: хук не должен мешать работе агента.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use serde_json::{json, Value};

use crate::clock;
use crate::json as j;
use crate::paths::Paths;

/// Срок просьбы о плане, если в запросе его нет: автоматическая просьба живёт 10 минут,
/// то, что отправлено вручную, — до 12 часов (их пишет монитор в поле ttl).
const TTL: f64 = 10.0 * 60e3;

/// Просьба о плане: что агент должен сделать. Текст дословный, по нему агенты уже пишут план.
const TEXT: &str = concat!(
    "Пользователь открыл монитор ccwatch и хочет видеть план этой сессии с оценкой времени. Называй ",
    "шаги понятным языком по сути работы, как для человека со стороны («Баланс механики яда», ",
    "«Тестирование боя»), а не внутренними кодами вроде «W6: HT1»; код, если он нужен, пиши в ",
    "скобках после названия. Если ты ведёшь план инструментом списка задач (update_plan, TodoWrite ",
    "или TaskCreate/TaskUpdate), обнови его один раз в таком виде, дописав в конец каждого шага ",
    "оценку времени вида «~15м» или «~1ч 30м», и продолжай как обычно. Если такого инструмента нет, ",
    "то в своём следующем сообщении, прямо перед очередным вызовом инструмента, напиши текстом ",
    "отдельную строку «ccwatch: план», под ней все шаги задачи с номерами и оценкой в конце («1. ",
    "Баланс механики яда (BAL4) ~20м»), а затем строку «ccwatch: ✓1 ▶2» с отметкой сделанного (✓) и ",
    "текущего шага (▶). Дальше в том сообщении, где берёшься за следующий шаг, добавляй к тексту ",
    "одну строку вида «ccwatch: ✓2 ▶3». Отдельных вызовов инструментов ради плана не делай и работу ",
    "не прерывай.",
);

/// Точка входа `ccwatch hook`: отдаёт агенту запрос сессии. Возвращает код выхода (всегда 0).
pub fn run(paths: &Paths) -> i32 {
    // быстрый путь: запросов нет — выходим, не читая stdin; хук стартует на каждый вызов инструмента
    if !has_asks(&paths.ask) {
        return 0;
    }
    let mut raw = Vec::new();
    if io::stdin().read_to_end(&mut raw).is_err() {
        return 0;
    }
    if let Some(out) = process(paths, &String::from_utf8_lossy(&raw)) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
    0
}

/// Есть ли в папке запросов хоть один *.json.
fn has_asks(dir: &Path) -> bool {
    fs::read_dir(dir)
        .is_ok_and(|rd| rd.filter_map(Result::ok).any(|e| e.file_name().to_string_lossy().ends_with(".json")))
}

/// Разбор входа хука (JSON с session_id и agent_id) и запроса сессии. Возвращает JSON для stdout
/// или None — агенту передать нечего. Запрос удаляется сразу, как в прежнем хуке; отметка о доставке
/// пишется в done/<id>.json, только если текст ушёл агенту.
pub fn process(paths: &Paths, raw: &str) -> Option<String> {
    let input: Value = serde_json::from_str(raw).ok()?;
    // запрос адресован главному агенту, вызовы инструментов субагентов пропускаем
    if !j::has(&input, "session_id") || j::has(&input, "agent_id") {
        return None;
    }
    // id — только буквы, цифры, _ и -: по нему строится имя файла
    let id: String =
        j::text(&input, "session_id").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    let file = paths.ask.join(format!("{id}.json"));
    let req: Value = serde_json::from_str(&String::from_utf8_lossy(&fs::read(&file).ok()?)).ok()?;
    fs::remove_file(&file).ok()?;

    let out = answer(&req)?;
    // отметка о доставке: все поля запроса и время доставки (порядок ключей сохраняется)
    let mut done = req;
    if let Some(m) = done.as_object_mut() {
        m.insert("deliveredAt".into(), json!(clock::now() as i64));
    }
    let _ = fs::create_dir_all(&paths.done);
    if let Ok(text) = serde_json::to_string(&done) {
        let _ = fs::write(paths.done.join(format!("{id}.json")), text);
    }
    Some(out)
}

/// Ответ на запрос: просьба о плане (если она есть) и советы, части через пустую строку — как JSON для stdout.
/// None — запрос просрочен или передавать нечего.
pub fn answer(req: &Value) -> Option<String> {
    let ttl = if j::has(req, "ttl") { j::number(req, "ttl") } else { TTL };
    if clock::now() - j::number(req, "at") > ttl {
        return None;
    }
    // советы — только непустые строки; старые запросы без полей — это просьба о плане
    let texts: Vec<String> = req
        .get("texts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    let plan = matches!(req.get("plan"), Some(Value::Bool(true))) || (req.get("plan").is_none() && texts.is_empty());
    let mut parts: Vec<String> = if plan { vec![TEXT.to_string()] } else { Vec::new() };
    parts.extend(texts);
    if parts.is_empty() {
        return None;
    }
    Some(
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": parts.join("\n\n")}})
            .to_string(),
    )
}
