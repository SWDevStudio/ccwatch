//! Оформление: цвета, жирный шрифт, шкала прогресса с переходом от розового к голубому.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::format::js_round;

/// Цвета включены, только когда вывод идёт в терминал и не задан NO_COLOR.
static COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_color(on: bool) {
    COLOR.store(on, Ordering::Relaxed);
}

pub fn color_on() -> bool {
    COLOR.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const PINK: Rgb = Rgb(255, 79, 163);
pub const CYAN: Rgb = Rgb(54, 226, 255);
pub const GREEN: Rgb = Rgb(90, 225, 150);
pub const AMBER: Rgb = Rgb(255, 190, 90);
pub const TEXT: Rgb = Rgb(230, 230, 245);
pub const DIM: Rgb = Rgb(125, 122, 150);
pub const FAINT: Rgb = Rgb(72, 70, 92);

/// Текст цветом rgb.
pub fn fg(c: Rgb, s: impl AsRef<str>) -> String {
    let s = s.as_ref();
    if color_on() {
        format!("\x1b[38;2;{};{};{}m{s}\x1b[39m", c.0, c.1, c.2)
    } else {
        s.to_string()
    }
}

pub fn bold(s: impl AsRef<str>) -> String {
    let s = s.as_ref();
    if color_on() {
        format!("\x1b[1m{s}\x1b[22m")
    } else {
        s.to_string()
    }
}

/// Цвета по назначению: c::pink(…) — акцент, c::text(…) — обычный текст, c::dim / c::faint — второстепенное.
pub mod c {
    use super::*;

    pub fn pink(s: impl AsRef<str>) -> String {
        fg(PINK, s)
    }
    pub fn cyan(s: impl AsRef<str>) -> String {
        fg(CYAN, s)
    }
    pub fn green(s: impl AsRef<str>) -> String {
        fg(GREEN, s)
    }
    pub fn amber(s: impl AsRef<str>) -> String {
        fg(AMBER, s)
    }
    pub fn text(s: impl AsRef<str>) -> String {
        fg(TEXT, s)
    }
    pub fn dim(s: impl AsRef<str>) -> String {
        fg(DIM, s)
    }
    pub fn faint(s: impl AsRef<str>) -> String {
        fg(FAINT, s)
    }
}

pub const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Шкала прогресса шириной w: заполненная часть с точностью до восьмой клетки.
pub fn bar(frac: f64, w: i64) -> String {
    let frac = if frac.is_nan() { 0.0 } else { frac.clamp(0.0, 1.0) };
    let cells = frac * w as f64;
    let full = cells.floor() as i64;
    let part = ((cells - full as f64) * 8.0).floor() as usize;
    let mut s = String::new();
    for i in 0..w {
        let t = if w > 1 { i as f64 / (w - 1) as f64 } else { 0.0 };
        let mix = |a: u8, b: u8| js_round(a as f64 + (b as f64 - a as f64) * t) as u8;
        let paint = Rgb(mix(PINK.0, CYAN.0), mix(PINK.1, CYAN.1), mix(PINK.2, CYAN.2));
        if i < full {
            s += &fg(paint, "█");
        } else if i == full && part > 0 {
            s += &fg(paint, EIGHTHS[part]);
        } else {
            s += &c::faint("─");
        }
    }
    s
}
