//! Реестр живых сессий: ~/.claude/sessions/<pid>.json — по нему видно, какие сессии запущены и в каком статусе.

use std::collections::HashSet;
use std::fs;
use std::io::Write;

use serde::Deserialize;

use crate::json::lenient;
use crate::paths::Paths;
use crate::re;
use crate::session::Session;

/// Запись реестра: процесс сессии, её id, папка и статус («busy», «idle» и т.п.).
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Registration {
    #[serde(deserialize_with = "lenient")]
    pub pid: u32,
    #[serde(deserialize_with = "lenient")]
    pub session_id: String,
    #[serde(deserialize_with = "lenient")]
    pub cwd: String,
    #[serde(deserialize_with = "lenient")]
    pub name: String,
    #[serde(deserialize_with = "lenient")]
    pub status: String,
    #[serde(deserialize_with = "lenient")]
    pub entrypoint: String,
    #[serde(deserialize_with = "lenient")]
    pub started_at: f64,
    #[serde(deserialize_with = "lenient")]
    pub status_updated_at: f64,
    #[serde(deserialize_with = "lenient")]
    pub updated_at: f64,
}

/// Жив ли процесс с таким pid.
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    /// Код выхода у процесса, который ещё работает.
    const STILL_ACTIVE: u32 = 259;

    if pid == 0 {
        return false;
    }
    // SAFETY: все дескрипторы закрываются сразу после использования.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // процесс есть, но к нему нет доступа — значит, он жив
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

/// Жив ли процесс с таким pid.
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: сигнал 0 только проверяет, что процесс существует.
    unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Читает реестр: файлы <число>.json в папке sessions, сортированные по имени.
/// Новые сессии добавляются в конец; сессии, которых больше нет в реестре, помечаются как не живые.
/// bell — звонок, когда сессия перестала быть занятой.
pub fn scan(paths: &Paths, sessions: &mut Vec<Session>, bell: bool) {
    let mut names: Vec<String> = fs::read_dir(&paths.sessions)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();

    let mut seen = HashSet::new();
    for name in names.iter().filter(|n| re!(r"^\d+\.json$").is_match(n)) {
        let Some(reg) = read_registration(&paths.sessions.join(name)) else { continue };
        if reg.session_id.is_empty() || !alive(reg.pid) {
            continue;
        }
        seen.insert(reg.session_id.clone());

        let idx = match sessions.iter().position(|s| s.id == reg.session_id) {
            Some(i) => i,
            None => {
                sessions.push(Session::new(&reg.session_id));
                sessions.len() - 1
            }
        };
        let s = &mut sessions[idx];
        let was_busy = s.reg.as_ref().is_some_and(|old| old.status == "busy");
        if bell && was_busy && reg.status != "busy" {
            print!("\x07");
            let _ = std::io::stdout().flush();
        }
        s.reg = Some(reg);
        s.live = true;
    }

    for s in sessions.iter_mut() {
        if !seen.contains(&s.id) {
            s.live = false;
        }
    }
}

/// Файл реестра; битый файл или не объект — None (как пропуск в JS).
fn read_registration(file: &std::path::Path) -> Option<Registration> {
    let bytes = fs::read(file).ok()?;
    serde_json::from_str(&String::from_utf8_lossy(&bytes)).ok()
}
