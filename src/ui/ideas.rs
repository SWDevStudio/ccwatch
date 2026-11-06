//! Экран идей для анализатора: вопрос агенту, его ответы этой сессии и копилка идей по всем сессиям.
//! Вид как у экрана скиллов: шапка с «←», пояснение, строки с прокруткой.

use crate::app::Monitor;
use crate::clock;
use crate::format::ago;
use crate::ideas::{Idea, StoredIdea};
use crate::requests::{AskState, IDEAS_KEY};
use crate::session::Session;
use crate::style::{bold, c};
use crate::text::{clip, spread, vis, wrap};

use super::{footer, Frame, Ui};

/// Пояснение в начале экрана: что за идеи и где копятся.
const INTRO: &str = "Агент предлагает правила для ccwatch по опыту своей работы: что отнимало время или токены и как это заметить по логам. Ответ агент записывает в ~/.ccwatch/ideas/<id сессии>.md, а копятся идеи в ~/.ccwatch/ideas.jsonl по всем сессиям.";

/// Экран идей: шапка, пояснение, состояние вопроса, идеи этой сессии и копилка других, с прокруткой.
pub fn ideas_view(m: &Monitor, ui: &mut Ui, s: &Session, w: i64, h: i64, now: f64) -> Frame {
    let inner = w - 4;
    let st = s.status(now);

    let title = format!("Идеи для анализатора · {}", clip(&s.name(), (inner - vis(&st.text) - 30).max(8)));
    let left = format!("  {} {}", c::dim("←"), bold(c::text(title)));
    let head = vec![spread(&left, &format!("{} {} ", st.icon, st.text), w), String::new()];

    let mut lines: Vec<String> = Vec::new();
    for l in wrap(INTRO, inner) {
        lines.push(format!("    {}", c::faint(l)));
    }
    lines.push(format!("    {}", ask_status(s, now)));
    lines.push(String::new());

    lines.push(format!("    {}", bold(c::text("Эта сессия"))));
    if s.ideas.is_empty() {
        lines.push(format!("    {}", c::faint("Пока нет.")));
    }
    // новые сверху; номер — по порядку на экране
    for (k, idea) in s.ideas.iter().rev().enumerate() {
        lines.push(idea_title(&format!("{}. {}", k + 1, idea.name), idea.t, w));
        lines.extend(detail_lines(&idea.detect, &idea.advice, inner));
    }
    lines.push(String::new());

    // копилка: идеи других сессий, новые сверху
    let mut others: Vec<&StoredIdea> = m.ideas.items.iter().filter(|x| x.session != s.id).collect();
    others.sort_by(|a, b| b.at.cmp(&a.at));
    lines.push(format!("    {}", bold(c::text(format!("Из других сессий · {}", others.len())))));
    if others.is_empty() {
        lines.push(format!("    {}", c::faint("Пока нет.")));
    }
    for x in others {
        let who = if x.project.is_empty() { x.name.clone() } else { format!("{} · {}", x.project, x.name) };
        lines.push(idea_title(&who, x.at as f64, w));
        lines.extend(detail_lines(&x.detect, &x.advice, inner));
    }

    let rows = (h - head.len() as i64 - 2).max(3);
    let max = (lines.len() as i64 - rows).max(0);
    ui.ideas_scroll = ui.ideas_scroll.max(0).min(max);
    let scroll = ui.ideas_scroll;
    let pos = if lines.len() as i64 > rows {
        format!("  ·  строки {}–{} из {}", scroll + 1, (scroll + rows).min(lines.len() as i64), lines.len())
    } else {
        String::new()
    };
    let mut body = head;
    body.extend(lines.iter().skip(scroll as usize).take(rows as usize).cloned());
    let text = format!("↑↓ прокрутка · Enter спросить агента · Esc назад · q выход{pos}");
    Frame { body, foot: footer(ui, &text, now) }
}

/// Строка о вопросе: ждёт доставки, дошёл и ждёт ответа, ответ (идеи или «идей нет») или ещё не задавали.
fn ask_status(s: &Session, now: f64) -> String {
    let asked = s.ask.as_ref().filter(|a| a.keys.iter().any(|k| k == IDEAS_KEY));
    if let Some(a) = asked.filter(|a| a.state == AskState::Wait) {
        return c::amber(format!("Вопрос отправлен {} назад, дойдёт при следующем действии агента.", ago(now - a.at)));
    }
    // когда вопрос дошёл: метка в транскрипте, а если её ещё не прочитали — отметка хука о доставке
    let delivered = s
        .main
        .as_ref()
        .and_then(|t| t.deliveries.iter().filter(|d| d.key == IDEAS_KEY).map(|d| d.t).reduce(f64::max))
        .or_else(|| asked.map(|a| a.at));
    let Some(dt) = delivered else {
        return c::faint("Вопрос не задавали. Enter — спросить агента.");
    };
    let answers: Vec<&Idea> = s.ideas.iter().filter(|x| x.t > dt).collect();
    if let Some(last) = answers.iter().map(|x| x.t).reduce(f64::max) {
        return c::green(format!("Агент ответил {} назад: идей {}.", ago(now - last), answers.len()));
    }
    if s.ideas_none_at > dt {
        return c::dim(format!("Агент ответил {} назад: идей нет.", ago(now - s.ideas_none_at)));
    }
    // файл появился или переписан после доставки — это ответ, даже если идей в нём не нашлось
    if s.ideas_file_at > dt {
        return c::dim(format!("Агент ответил {} назад: идей нет.", ago(now - s.ideas_file_at)));
    }
    c::amber(format!("Вопрос дошёл {} назад, ответ агент запишет в файл.", ago(now - dt)))
}

/// Название идеи слева и время её строки справа.
fn idea_title(name: &str, t: f64, w: i64) -> String {
    let left = format!("    {}", c::text(clip(name, w - 20)));
    spread(&left, &c::faint(clock::clock(t)), w)
}

/// Подписи «Как заметить» и «Что советовать» под идеей; пустые части не выводим.
fn detail_lines(detect: &str, advice: &str, inner: i64) -> Vec<String> {
    let mut out = Vec::new();
    for (label, text) in [("Как заметить: ", detect), ("Что советовать: ", advice)] {
        if text.is_empty() {
            continue;
        }
        for l in wrap(&format!("{label}{text}"), inner - 6) {
            out.push(format!("      {}", c::dim(l)));
        }
    }
    out
}
