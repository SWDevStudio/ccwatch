//! Таблица процессов Windows через системный API, без PowerShell: снимок процессов,
//! память и время создания каждого процесса, заголовки видимых окон.

use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows_sys::Win32::Foundation::{CloseHandle, BOOL, FILETIME, HWND, INVALID_HANDLE_VALUE, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
};

use super::Proc;
use crate::registry::alive;

/// Флаг CREATE_NO_WINDOW: taskkill не открывает окно консоли.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Разница между эпохой FILETIME (1601) и Unix-эпохой, в миллисекундах.
const EPOCH_DIFF_MS: u64 = 11_644_473_600_000;

/// Все процессы: родитель, имя, память, время создания и заголовок главного окна.
/// Процесс, который не открылся (нет прав), остаётся с памятью 0 и временем 0.
pub fn table() -> HashMap<u32, Proc> {
    let titles = window_titles();
    let mut procs = HashMap::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return procs;
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        while more {
            let pid = entry.th32ProcessID;
            let mut p =
                Proc { ppid: entry.th32ParentProcessID, name: wide_to_string(&entry.szExeFile), ..Proc::default() };
            read_process_info(pid, &mut p);
            p.title = titles.get(&pid).cloned().unwrap_or_default();
            procs.insert(pid, p);
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    procs
}

/// Память рабочего набора и время создания процесса. Если процесс не открылся — поля остаются 0.
fn read_process_info(pid: u32, p: &mut Proc) {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0 {
            p.mem = counters.WorkingSetSize as f64;
        }
        let mut created: FILETIME = zeroed();
        let mut exited: FILETIME = zeroed();
        let mut kernel: FILETIME = zeroed();
        let mut user: FILETIME = zeroed();
        if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0 {
            p.started = filetime_ms(&created);
        }
        CloseHandle(handle);
    }
}

/// FILETIME (100-нс интервалы от 1601) → мс от 1970, как Date в JS.
fn filetime_ms(ft: &FILETIME) -> f64 {
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    if ticks == 0 {
        return 0.0;
    }
    (ticks / 10_000 - EPOCH_DIFF_MS) as f64
}

/// Первый непустой заголовок видимого окна без владельца для каждого процесса —
/// это то, что .NET отдаёт как MainWindowTitle.
fn window_titles() -> HashMap<u32, String> {
    let mut titles: HashMap<u32, String> = HashMap::new();
    unsafe {
        EnumWindows(Some(collect_title), &mut titles as *mut HashMap<u32, String> as LPARAM);
    }
    titles
}

/// Колбэк EnumWindows: запоминает заголовок окна процесса; возвращает 1, чтобы перебор шёл дальше.
unsafe extern "system" fn collect_title(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
        return 1;
    }
    let titles = &mut *(lparam as *mut HashMap<u32, String>);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if titles.contains_key(&pid) {
        return 1;
    }
    let mut buf = [0u16; 512];
    let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if len > 0 {
        titles.insert(pid, String::from_utf16_lossy(&buf[..len as usize]));
    }
    1
}

/// Строка UTF-16 из буфера с нулём в конце (как szExeFile).
fn wide_to_string(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// Завершает процесс pid вместе с дочерними (taskkill /T /F) в отдельном потоке, чтобы не держать экран.
/// Ошибку показываем, только если процесс после попытки всё ещё жив.
pub fn kill_tree(pid: u32, ok: String, done: Box<dyn FnOnce(String) + Send>) {
    std::thread::spawn(move || {
        let out = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        let msg = match out {
            Ok(o) if o.status.success() => ok,
            Ok(o) if alive(pid) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                format!("Не удалось завершить: Command failed: taskkill /PID {pid} /T /F\n{}", stderr.trim_end())
            }
            Err(e) if alive(pid) => format!("Не удалось завершить: {e}"),
            _ => ok,
        };
        done(msg);
    });
}
