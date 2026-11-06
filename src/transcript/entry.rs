//! Строка транскрипта — только поля, которые нужны монитору. Остальное serde пропускает,
//! а поле неожиданного типа становится пустым, не роняя всю строку.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::clock;
use crate::json::{self, lenient};
use crate::paths::basename;

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Entry {
    /// "assistant", "user", "ai-title", "attachment"…
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub kind: String,
    #[serde(deserialize_with = "lenient")]
    pub uuid: String,
    #[serde(deserialize_with = "lenient")]
    pub timestamp: String,
    /// строка субагента внутри главного транскрипта
    #[serde(rename = "isSidechain", deserialize_with = "lenient")]
    pub is_sidechain: bool,
    /// уровень effort, с которым шёл запрос
    #[serde(deserialize_with = "lenient")]
    pub effort: String,
    #[serde(rename = "thinkingDurationMs", deserialize_with = "lenient")]
    pub thinking_ms: f64,
    #[serde(rename = "aiTitle", deserialize_with = "lenient")]
    pub ai_title: String,
    #[serde(deserialize_with = "lenient")]
    pub message: Option<Message>,
    /// ответ инструмента для интерфейса: у TaskCreate в нём номер задачи
    #[serde(rename = "toolUseResult")]
    pub tool_use_result: Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Message {
    /// одно сообщение модели пишется несколькими строками с одним id
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    #[serde(deserialize_with = "lenient")]
    pub model: String,
    #[serde(deserialize_with = "lenient")]
    pub usage: Option<Usage>,
    #[serde(deserialize_with = "blocks")]
    pub content: Vec<Block>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Usage {
    #[serde(deserialize_with = "lenient")]
    pub input_tokens: f64,
    #[serde(deserialize_with = "lenient")]
    pub cache_creation_input_tokens: f64,
    #[serde(deserialize_with = "lenient")]
    pub cache_read_input_tokens: f64,
    #[serde(deserialize_with = "lenient")]
    pub output_tokens: f64,
    #[serde(deserialize_with = "lenient")]
    pub cache_creation: CacheCreation,
}

/// Запись в кэш по сроку жизни: на час дороже, чем на 5 минут.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct CacheCreation {
    #[serde(deserialize_with = "lenient")]
    pub ephemeral_1h_input_tokens: f64,
    #[serde(deserialize_with = "lenient")]
    pub ephemeral_5m_input_tokens: Option<f64>,
}

impl Usage {
    /// размер контекста запроса: новый вход, запись в кэш и чтение из кэша
    pub fn context(&self) -> f64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }

    /// записано в кэш на 5 минут и на час
    pub fn writes(&self) -> (f64, f64) {
        let h1 = self.cache_creation.ephemeral_1h_input_tokens;
        let m5 = self.cache_creation.ephemeral_5m_input_tokens.unwrap_or(self.cache_creation_input_tokens - h1);
        (m5, h1)
    }
}

/// Кусок содержимого сообщения: текст, вызов инструмента или его результат.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Block {
    #[serde(rename = "type", deserialize_with = "lenient")]
    pub kind: String,
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    /// параметры вызова инструмента
    pub input: Value,
    pub text: Value,
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: String,
}

/// Содержимое сообщения бывает и строкой — тогда кусков нет.
fn blocks<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Block>, D::Error> {
    let v = Value::deserialize(d)?;
    let Value::Array(items) = v else { return Ok(Vec::new()) };
    Ok(items.into_iter().filter_map(|x| Block::deserialize(x).ok()).collect())
}

impl Entry {
    pub fn time(&self) -> f64 {
        clock::parse_iso(&self.timestamp).unwrap_or(0.0)
    }

    /// Вызовы инструментов в ответе модели.
    pub fn tool_uses(&self) -> impl Iterator<Item = &Block> {
        let content = match (&self.message, self.kind.as_str()) {
            (Some(m), "assistant") => m.content.as_slice(),
            _ => &[],
        };
        content.iter().filter(|b| b.kind == "tool_use")
    }
}

/// Действие агента для строки «Сейчас»: инструмент, его суть и когда.
#[derive(Clone, Debug)]
pub struct Action {
    pub tool: String,
    pub detail: String,
    pub at: f64,
}

impl Action {
    pub fn of(b: &Block, timestamp: &str) -> Action {
        Action {
            tool: tool_name(&b.name),
            detail: tool_detail(&b.input),
            at: clock::parse_iso(timestamp).filter(|t| *t != 0.0).unwrap_or_else(clock::now),
        }
    }

    pub fn describe(&self) -> String {
        if self.detail.is_empty() {
            self.tool.clone()
        } else {
            format!("{} · {}", self.tool, self.detail)
        }
    }
}

/// «mcp__сервер__инструмент» → «инструмент».
pub fn tool_name(n: &str) -> String {
    if !n.starts_with("mcp__") {
        return n.to_string();
    }
    let p: Vec<&str> = n.split("__").collect();
    let rest = p[2..].join("__");
    if rest.is_empty() {
        p.get(1).unwrap_or(&"").to_string()
    } else {
        rest
    }
}

/// Суть вызова: описание, имя файла, команда, шаблон поиска…
pub fn tool_detail(i: &Value) -> String {
    let file = json::has(i, "file_path").then(|| basename(&json::text(i, "file_path")));
    let v = [json::first_truthy(i, &["description"])]
        .into_iter()
        .chain(file.filter(|f| !f.is_empty()))
        .chain([json::first_truthy(i, &["command", "pattern", "query", "url", "prompt", "action"])])
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    collapse_spaces(&v)
}

/// Пробелы и переводы строк — одним пробелом, по краям — без них.
pub fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
