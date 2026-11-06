//! История скиллов: сводка по скиллам (кто и сколько раз) и лента вызовов, новые сверху.

use crate::clock;
use crate::format::times;
use crate::session::Session;
use crate::style::{bold, c};
use crate::text::{clip, pad, pad_start, spread, vis};
use crate::tips::{is_builtin, SkillCount};
use crate::transcript::By;

use super::{footer, Frame, Ui};

/// Экран скиллов: шапка со счётчиками, затем разделы «По скиллам» и «История» с прокруткой.
pub fn skills_view(ui: &mut Ui, s: &Session, w: i64, h: i64, now: f64) -> Frame {
    let inner = w - 4;
    let st = s.status(now);
    let u = s.skill_use();

    let title = format!("Скиллы · {}", clip(&s.name(), inner - vis(&st.text) - 16));
    let left = format!("  {} {}", c::dim("←"), bold(c::text(title)));
    let mut summary =
        format!("агенты вызвали скиллы {} · вы набрали скиллов {}", times(u.agent as i64), times(u.user_skills as i64));
    if u.builtins > 0 {
        summary += &format!(" и встроенных команд {}", u.builtins);
    }
    let head = vec![
        spread(&left, &format!("{} {} ", st.icon, st.text), w),
        format!("    {}", c::cyan(clip(&summary, inner))),
        String::new(),
    ];

    let mut lines: Vec<String> = Vec::new();
    if !u.by_skill.is_empty() {
        lines.push(format!("    {}", bold(c::text("По скиллам"))));
        lines.extend(by_skill_lines(&u.by_skill, inner));
        lines.push(String::new());
    }
    lines.push(format!("    {}{}", bold(c::text("История")), c::faint(" · новые сверху")));
    if u.events.is_empty() {
        lines.push(format!("    {}", c::faint("В логах этой сессии скиллов пока нет.")));
    }
    for e in &u.events {
        let kind = if e.by == By::User {
            if is_builtin(&e.name) {
                c::faint("команда")
            } else {
                c::amber("/скилл ")
            }
        } else {
            c::green("скилл  ")
        };
        let args = if e.args.is_empty() { String::new() } else { format!(" {}", squeeze_spaces(&e.args)) };
        let when = c::faint(format!("{}  ", pad_start(&clock::clock(e.t), 11)));
        let what = c::text(clip(&format!("{}{args}", e.name), 40));
        let who = c::dim(format!("  {}", clip(&e.who, inner - 64)));
        lines.push(format!("    {when}{kind} {what}{who}"));
    }

    let rows = (h - head.len() as i64 - 2).max(3);
    let max = (lines.len() as i64 - rows).max(0);
    ui.sk_scroll = ui.sk_scroll.max(0).min(max);
    let scroll = ui.sk_scroll;
    let pos = if lines.len() as i64 > rows {
        format!("  ·  строки {}–{} из {}", scroll + 1, (scroll + rows).min(lines.len() as i64), lines.len())
    } else {
        String::new()
    };
    let mut body = head;
    body.extend(lines.iter().skip(scroll as usize).take(rows as usize).cloned());
    let text = format!("↑↓ прокрутка · Esc назад · q выход{pos}");
    Frame { body, foot: footer(ui, &text, now) }
}

/// Строки раздела «По скиллам»: имя, число вызовов и кто вызывал, самые частые выше.
fn by_skill_lines(by_skill: &indexmap::IndexMap<String, SkillCount>, inner: i64) -> Vec<String> {
    let mut sorted: Vec<(&String, &SkillCount)> = by_skill.iter().collect();
    sorted.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    sorted
        .into_iter()
        .map(|(name, r)| {
            let mut who: Vec<(&String, &usize)> = r.who.iter().collect();
            who.sort_by(|a, b| b.1.cmp(a.1));
            let who_text = who.iter().map(|(who_name, n)| format!("{who_name} ×{n}")).collect::<Vec<_>>().join(", ");
            let name_cell = c::text(pad(&clip(name, 30), 31));
            let count_cell = c::cyan(format!("{}  ", pad_start(&r.n.to_string(), 4)));
            format!("    {name_cell}{count_cell}{}", c::dim(clip(&who_text, inner - 37)))
        })
        .collect()
}

/// Аргументы скилла: каждая группа пробельных символов — один пробел, края не трогаем (как replace(/\s+/g, ' ')).
fn squeeze_spaces(s: &str) -> String {
    let mut out = String::new();
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out
}
