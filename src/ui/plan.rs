//! Весь план: каждый шаг с номером и полным текстом (длинный переносится), прокрутка ↑↓ / PgUp / PgDn.

use crate::clock;
use crate::format::{approx, dur, js_round, num};
use crate::session::Session;
use crate::style::{bold, c};
use crate::text::{clip, pad_start, spread, vis, wrap};

use super::detail::{step_mark, step_text, step_time};
use super::{footer, Frame, Ui};

/// Экран плана: шапка со сводкой, затем шаги с прокруткой; при открытии — к текущему шагу.
pub fn plan_view(ui: &mut Ui, s: &Session, w: i64, h: i64, now: f64) -> Frame {
    let inner = w - 4;
    let st = s.status(now);
    let p = s.progress();
    let e = s.estimate(now);

    let title = format!("План · {}", clip(&s.name(), inner - vis(&st.text) - 14));
    let left = format!("  {} {}", c::dim("←"), bold(c::text(title)));
    let mut head = vec![spread(&left, &format!("{} {} ", st.icon, st.text), w)];

    let mut summary: Vec<String> = Vec::new();
    if p.n > 0 {
        summary.push(format!("{}%", num(js_round(p.done as f64 / p.n as f64 * 100.0))));
        summary.push(match p.cur {
            Some(_) => format!("шаг {} из {}", p.idx, p.n),
            None => format!("{} из {}", p.done, p.n),
        });
    }
    if let Some(e) = &e {
        if e.all_done {
            summary.push(format!("оценка {}", approx(e.total)));
            summary.push(format!("заняло {}", dur(e.spent)));
        } else {
            summary.push(format!("оценка {}", approx(e.total)));
            summary.push(format!("осталось {}", approx(e.left)));
            summary.push(format!("закончит ≈ {}", clock::clock(now + e.left)));
        }
    }
    head.push(format!("    {}", c::cyan(clip(&summary.join(" · "), inner))));
    head.push(String::new());

    let (lines, cur_line) = plan_lines(s, inner, w, now);
    let lines =
        if lines.is_empty() { vec![format!("    {}", c::faint("План не ведётся."))] } else { lines };

    let rows = (h - head.len() as i64 - 2).max(3);
    let max = (lines.len() as i64 - rows).max(0);
    // при открытии показываем текущий шаг чуть ниже верха экрана
    if ui.plan_scroll.is_none() {
        ui.plan_scroll = Some((cur_line as i64 - rows.div_euclid(3)).max(0));
    }
    let scroll = ui.plan_scroll.unwrap_or(0).max(0).min(max);
    ui.plan_scroll = Some(scroll);
    ui.plan_page = rows;

    let shown: Vec<String> = lines.iter().skip(scroll as usize).take(rows as usize).cloned().collect();
    let pos = if lines.len() as i64 > rows {
        format!("  ·  строки {}–{} из {}", scroll + 1, scroll + shown.len() as i64, lines.len())
    } else {
        String::new()
    };
    head.extend(shown);
    let text = format!("↑↓ прокрутка · PgUp/PgDn · Esc назад · q выход{pos}");
    Frame { body: head, foot: footer(ui, &text, now) }
}

/// Строки всех шагов с переносом; заодно номер строки, где стоит шаг в работе.
fn plan_lines(s: &Session, inner: i64, w: i64, now: f64) -> (Vec<String>, usize) {
    let mut lines: Vec<String> = Vec::new();
    let mut cur_line = 0usize;
    let num_w = s.plan.len().to_string().len();
    for (k, x) in s.plan.iter().enumerate() {
        let right = step_time(x, now);
        let prefix = format!("{}  ", pad_start(&(k + 1).to_string(), num_w));
        let right_room = if right.is_empty() { 0 } else { vis(&right) + 2 };
        let width = (inner - num_w as i64 - 6 - right_room).max(10);
        let parts = wrap(&x.content, width);
        if matches!(x.status, crate::session::Status::InProgress) {
            cur_line = lines.len();
        }
        for (j, part) in parts.iter().enumerate() {
            let left = if j > 0 {
                format!("    {}   {}", " ".repeat(num_w + 2), step_text(x, part))
            } else {
                format!("    {}{}  {}", c::faint(&prefix), step_mark(x), step_text(x, part))
            };
            lines.push(if j == 0 && !right.is_empty() { spread(&left, &format!("{right}  "), w) } else { left });
        }
    }
    (lines, cur_line)
}
