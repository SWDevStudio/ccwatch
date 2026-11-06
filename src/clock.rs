//! Время: «сейчас», разбор отметок из транскриптов и местное время для экрана.
//! Все моменты — миллисекунды от 1970 года в f64, как Date.now() в Node-версии.

use std::fs::Metadata;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};

/// Замороженное «сейчас» для сверки кадров с эталонными снимками; 0 — настоящее время.
static FROZEN: AtomicU64 = AtomicU64::new(0);

pub fn freeze(ms: f64) {
    FROZEN.store(ms as u64, Ordering::Relaxed);
}

pub fn now() -> f64 {
    let frozen = FROZEN.load(Ordering::Relaxed);
    if frozen != 0 {
        return frozen as f64;
    }
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// Время изменения файла с долями миллисекунды, как mtimeMs.
pub fn mtime_ms(meta: &Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_micros() as f64 / 1000.0)
        .unwrap_or(0.0)
}

/// Date.parse: «2026-10-18T00:59:12.345Z» → миллисекунды; без часового пояса — местное время.
pub fn parse_iso(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp_millis() as f64);
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    Local.from_local_datetime(&naive).earliest().map(|t| t.timestamp_millis() as f64)
}

fn local(t: f64) -> DateTime<Local> {
    Local.timestamp_millis_opt(t as i64).single().unwrap_or_else(|| Local.timestamp_millis_opt(0).unwrap())
}

/// Время окончания: «04:57»; если это не сегодня — с датой «19.10 04:57»,
/// иначе «04:57» при остатке в сутки сбивает с толку.
pub fn clock(t: f64) -> String {
    let d = local(t);
    let hm = d.format("%H:%M").to_string();
    if d.date_naive() == local(now()).date_naive() {
        hm
    } else {
        format!("{} {hm}", d.format("%d.%m"))
    }
}

/// Часы в шапке: «04:57:12».
pub fn time_of_day(t: f64) -> String {
    local(t).format("%H:%M:%S").to_string()
}
