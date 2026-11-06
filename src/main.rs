//! ccwatch — монитор сессий Claude Code: живой режим в терминале или один снимок (--once, не терминал).

use std::env;
use std::io::{stdin, stdout, IsTerminal};

use ccwatch::app::App;
use ccwatch::cli;
use ccwatch::hook;
use ccwatch::paths::Paths;
use ccwatch::style;
use ccwatch::ui::live;

/// Ширина терминала для снимка; если её не узнать — 100 столбцов, как в Node-версии.
fn terminal_width() -> i64 {
    crossterm::terminal::size().map(|(cols, _)| i64::from(cols)).ok().filter(|&cols| cols > 0).unwrap_or(100)
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    // хук для Claude Code: его вызывает сам Claude Code, поэтому разбираем его раньше остальных аргументов
    if args.first().is_some_and(|a| a == "hook") {
        std::process::exit(hook::run(&Paths::from_env()));
    }
    let opts = cli::parse(&args);
    if opts.help {
        println!("{}", cli::HELP);
        return Ok(());
    }
    // живой режим — только в терминале; в трубе или перенаправлении печатаем снимок
    let tty = stdout().is_terminal() && stdin().is_terminal();
    let once = opts.once || !tty;
    // цвета — только в терминале и без переменной NO_COLOR (пустая не считается)
    let no_color = env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    style::set_color(stdout().is_terminal() && !no_color);

    let mut app = App::start(Paths::from_env(), opts, once);
    if once {
        let width = if stdout().is_terminal() { terminal_width() } else { 100 };
        println!("{}", app.snapshot(width));
        app.quit();
        return Ok(());
    }
    live::run(app)
}
