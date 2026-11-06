//! Живой режим: терминал в альтернативном экране, клавиши, перерисовка и обход сессий раз в секунду.
//! Перерисовка — каждые 120 мс и после клавиши; закрытие окна консоли (или SIGHUP / SIGTERM)
//! не должно оставить запросы плана в папке ожидания, поэтому выход идёт через app.quit().

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode};

use crate::app::App;
use crate::ui::keys::Key;

const DRAW_EVERY: Duration = Duration::from_millis(120);
const TICK_EVERY: Duration = Duration::from_millis(1000);

/// Пришло закрытие окна консоли, Ctrl+C или сигнал завершения: цикл пора заканчивать.
static CLOSING: AtomicBool = AtomicBool::new(false);
/// Терминал восстановлен и запросы убраны: обработчик закрытия окна может отпускать процесс.
static DONE: AtomicBool = AtomicBool::new(false);

/// Запускает живой режим и возвращается, когда пользователь вышел (или окно закрыли).
pub fn run(mut app: App) -> io::Result<()> {
    enable_vt_output();
    install_close_handler();
    // при панике сначала возвращаем терминал, потом показываем сообщение об ошибке
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut out = io::stdout();
    out.write_all(b"\x1b[?1049h\x1b[?25l")?;
    out.flush()?;

    let result = event_loop(&mut app);

    app.quit();
    restore_terminal();
    DONE.store(true, Ordering::SeqCst);
    result
}

/// Цикл: по сроку отрисовки и обхода, между ними ждём клавиш и изменения размера окна.
fn event_loop(app: &mut App) -> io::Result<()> {
    let mut next_tick = Instant::now() + TICK_EVERY;
    let mut next_draw = Instant::now();
    loop {
        if CLOSING.load(Ordering::SeqCst) {
            return Ok(());
        }
        let now = Instant::now();
        if now >= next_tick {
            app.tick();
            next_tick = now + TICK_EVERY;
        }
        if now >= next_draw {
            draw(app)?;
            next_draw = now + DRAW_EVERY;
        }
        let wait = next_tick.min(next_draw).saturating_duration_since(Instant::now());
        if !event::poll(wait)? {
            continue;
        }
        match event::read()? {
            // отпускание клавиши (на Windows приходит отдельно) нам не нужно
            Event::Key(ev) if ev.kind == KeyEventKind::Press => {
                if app.on_key(map_key(ev)) {
                    return Ok(());
                }
                draw(app)?;
            }
            Event::Resize(..) => draw(app)?,
            _ => {}
        }
    }
}

/// Перерисовка без мерцания: курсор в начало, каждая строка с очисткой хвоста, очистка низа экрана.
fn draw(app: &mut App) -> io::Result<()> {
    app.poll_kills();
    let (cols, rows) = terminal::size().unwrap_or((100, 30));
    let w = i64::from(cols).max(40);
    let h = i64::from(rows).max(10);
    let lines = app.screen(w, h);
    let mut frame = String::from("\x1b[H");
    frame.push_str(&lines.iter().map(|l| format!("{l}\x1b[K")).collect::<Vec<_>>().join("\n"));
    frame.push_str("\x1b[J");
    let mut out = io::stdout();
    out.write_all(frame.as_bytes())?;
    out.flush()
}

/// Возвращает терминал в обычный режим: выход из альтернативного экрана, курсор, без raw-режима.
fn restore_terminal() {
    let mut out = io::stdout();
    let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
    let _ = out.flush();
    let _ = disable_raw_mode();
}

/// Перевод события crossterm в клавишу монитора.
pub fn map_key(ev: KeyEvent) -> Key {
    let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
    match ev.code {
        // Ctrl+буква идёт через раскладку активного окна: при русской ЙЦУКЕН Ctrl+C приходит как «с», Ctrl+U — как «г»
        KeyCode::Char('c' | 'C' | 'с' | 'С') if ctrl => Key::CtrlC,
        KeyCode::Char('u' | 'U' | 'г' | 'Г') if ctrl => Key::CtrlU,
        // через ConPTY Ctrl+C и Ctrl+U приходят управляющими символами, без модификатора
        KeyCode::Char('\u{3}') => Key::CtrlC,
        KeyCode::Char('\u{15}') => Key::CtrlU,
        KeyCode::Char(_) if ctrl => Key::Other,
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => Key::Other,
    }
}

/// Windows: управляющие последовательности (цвета, курсор) понимаются, только если включён VT-вывод.
#[cfg(windows)]
fn enable_vt_output() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
    };
    // SAFETY: WinAPI только читает и меняет режим консоли стандартного вывода
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_vt_output() {}

/// Windows: закрытие окна консоли приходит обработчику в отдельном потоке.
#[cfg(windows)]
fn install_close_handler() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    // SAFETY: регистрируем обработчик с правильной сигнатурой; снимать его не нужно — процесс сам завершится
    unsafe {
        SetConsoleCtrlHandler(Some(on_console_event), 1);
    }
}

#[cfg(windows)]
unsafe extern "system" fn on_console_event(event: u32) -> i32 {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };
    // Ctrl+C и Ctrl+Break — просьба выйти наравне с закрытием окна: процесс не убиваем, цикл выходит сам
    let wanted = [CTRL_C_EVENT, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT];
    if !wanted.contains(&event) {
        return 0;
    }
    CLOSING.store(true, Ordering::SeqCst);
    // основной поток восстановит терминал и уберёт запросы; ждём его не дольше 3 секунд
    let started = Instant::now();
    while !DONE.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    1
}

/// Unix: SIGHUP (закрыли терминал) и SIGTERM — тот же путь, что закрытие окна.
#[cfg(unix)]
fn install_close_handler() {
    extern "C" fn on_signal(_sig: libc::c_int) {
        CLOSING.store(true, Ordering::SeqCst);
    }
    let handler: libc::sighandler_t = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: обработчик только выставляет атомарный флаг — это безопасно в обработчике сигнала
    unsafe {
        libc::signal(libc::SIGHUP, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

#[cfg(not(any(unix, windows)))]
fn install_close_handler() {}
