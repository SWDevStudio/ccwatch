//! Таблица процессов для macOS и Linux: `ps -A -o pid=,ppid=,rss=,lstart=,comm=`.
//! Заголовков окон здесь нет, поэтому title пустой.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
use regex::Regex;

use super::Proc;
use crate::paths::basename;
use crate::pattern;

/// Строка вывода ps: pid, ppid, rss (КБ), время запуска «Sat Oct 18 10:00:00 2026», имя команды.
static LINE: LazyLock<Regex> =
    LazyLock::new(|| pattern::js(r"^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\w{3} \w{3}\s+\d+ [\d:]+ \d{4})\s+(.+)$", ""));

/// Все процессы системы; при ошибке запуска ps — пустая таблица (как catch в JS).
pub fn table() -> HashMap<u32, Proc> {
    let out = Command::new("ps").args(["-A", "-o", "pid=,ppid=,rss=,lstart=,comm="]).output();
    match out {
        Ok(o) if o.status.success() => parse(&String::from_utf8_lossy(&o.stdout)),
        _ => HashMap::new(),
    }
}

fn parse(out: &str) -> HashMap<u32, Proc> {
    let mut procs = HashMap::new();
    for line in out.lines() {
        let Some(m) = LINE.captures(line) else { continue };
        let pid: u32 = m[1].parse().unwrap_or(0);
        let ppid: u32 = m[2].parse().unwrap_or(0);
        let rss_kb: f64 = m[3].parse().unwrap_or(0.0);
        procs.insert(
            pid,
            Proc {
                ppid,
                name: basename(m[5].trim()),
                mem: rss_kb * 1024.0,
                started: parse_lstart(&m[4]),
                title: String::new(),
            },
        );
    }
    procs
}

/// Время из вывода ps («Sat Oct 18 10:00:00 2026», местное время) → мс от 1970; 0, если не разобрать.
fn parse_lstart(s: &str) -> f64 {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return 0.0;
    }
    let Some(month) = MONTHS.iter().position(|m| *m == parts[1]) else { return 0.0 };
    let day: u32 = parts[2].parse().unwrap_or(0);
    let year: i32 = parts[4].parse().unwrap_or(0);
    let hms: Vec<u32> = parts[3].split(':').filter_map(|x| x.parse().ok()).collect();
    if hms.len() != 3 {
        return 0.0;
    }
    let date = NaiveDate::from_ymd_opt(year, month as u32 + 1, day);
    let time = NaiveTime::from_hms_opt(hms[0], hms[1], hms[2]);
    match (date, time) {
        (Some(d), Some(t)) => {
            Local.from_local_datetime(&d.and_time(t)).earliest().map(|dt| dt.timestamp_millis() as f64).unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// Завершает процесс pid и его дочерние: сначала `pkill -TERM -P pid`, потом SIGTERM самому процессу.
/// Как в Node-версии, это делается синхронно.
pub fn kill_tree(pid: u32, ok: String, done: Box<dyn FnOnce(String) + Send>) {
    let _ = Command::new("pkill")
        .args(["-TERM", "-P", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc == 0 {
        done(ok);
    } else {
        done(format!("Не удалось завершить: {}", std::io::Error::last_os_error()));
    }
}
