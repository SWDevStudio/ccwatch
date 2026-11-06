//! Поля JSON так, как их читала Node-версия: пустая строка, 0, false и null — «нет значения»,
//! а поле неожиданного типа не роняет разбор всей строки.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Значение «истинно» по правилам JS: не null, не false, не 0, не пустая строка.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// String(v) из JS.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map(crate::format::num).unwrap_or_default(),
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            a.iter().map(|x| if x.is_null() { String::new() } else { js_string(x) }).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) => "[object Object]".into(),
    }
}

/// Поле как текст: нет поля или null — пустая строка (в JS `String(x ?? '')`).
pub fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(x) => js_string(x),
    }
}

/// Первое «истинное» из полей — как `a.x || a.y || …` в JS; ни одного — пустая строка.
pub fn first_truthy(v: &Value, keys: &[&str]) -> String {
    keys.iter().filter_map(|k| v.get(*k)).find(|x| truthy(x)).map(js_string).unwrap_or_default()
}

/// Есть ли «истинное» поле.
pub fn has(v: &Value, key: &str) -> bool {
    v.get(key).is_some_and(truthy)
}

/// Number(x) || 0 из JS: число или строка с числом, иначе 0.
pub fn number(v: &Value, key: &str) -> f64 {
    let n = match v.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        Some(Value::Bool(true)) => 1.0,
        _ => 0.0,
    };
    if n.is_nan() {
        0.0
    } else {
        n
    }
}

/// Для полей serde: значение неожиданного типа превращается в значение по умолчанию.
pub fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(T::deserialize(v).unwrap_or_default())
}

/// Равенство значений как `===` в JS для простых значений: 400000 и 400000.0 — одно и то же.
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}
