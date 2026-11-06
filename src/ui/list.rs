//! Список сессий: карточка на каждую сессию (три строки) и прокрутка, если карточки не влезают.

use crate::app::Monitor;
use crate::clock;
use crate::format::{approx, dur, mb, tok};
use crate::paths::basename;
use crate::requests::AskState;
use crate::session::{short_model, Agent, Session};
use crate::style::{bar, bold, c};
use crate::text::{clip, pad_start, spread, vis};

use super::{footer, header, Frame, Ui};

/// Карточка сессии: заголовок со статусом, затем прогресс плана или последнее действие, затем сводка.
fn card(m: &Monitor, s: &Session, w: i64, now: f64, selected: bool) -> [String; 3] {
    let g = if selected { format!(" {} ", c::pink("▌")) } else { "   ".to_string() };
    let inner = w - 4;
    let st = s.status(now);

    let name = clip(&s.name(), inner - 2 - vis(&st.text) - 2);
    let name_styled = if selected { bold(c::text(&name)) } else { c::text(&name) };
    let l1 = spread(&format!("{g}{} {name_styled}", st.icon), &format!("{} ", st.text), w);

    let l2 = if s.main.is_none() {
        format!("{g}  {}", c::faint("транскрипт не найден"))
    } else if s.progress().n > 0 {
        progress_line(s, &g, w, inner, now)
    } else {
        activity_line(s, &g, inner)
    };

    [l1, l2, meta_line(m, s, &g, inner)]
}

/// Вторая строка карточки, когда план есть: шкала, счёт, текущий шаг и оценка справа.
fn progress_line(s: &Session, g: &str, w: i64, inner: i64, now: f64) -> String {
    let p = s.progress();
    // шкала от 8 до 24 клеток, в узкой карточке — короче
    let bw = inner.div_euclid(4).max(8).min(24);
    let what = match p.cur {
        Some(k) => s.plan.get(k).map(|x| x.content.clone()).unwrap_or_default(),
        None if p.done == p.n => "все шаги выполнены".to_string(),
        None => String::new(),
    };
    let right = match s.estimate(now) {
        None => String::new(),
        Some(e) if e.all_done => c::dim(format!("за {} при оценке {} ", dur(e.spent), approx(e.total))),
        Some(e) => {
            let txt = format!("осталось {} · ≈{} ", approx(e.left), clock::clock(now + e.left));
            if e.over.is_some() {
                c::amber(txt)
            } else {
                c::cyan(txt)
            }
        }
    };
    let counter = c::text(pad_start(&format!("{}/{}", p.done, p.n), 6));
    let left = format!("{g}  {}{counter}  ", bar(p.done as f64 / p.n as f64, bw));
    let what_room = w - vis(&left) - vis(&right) - 2;
    spread(&format!("{left}{}", c::dim(clip(&what, what_room))), &right, w)
}

/// Вторая строка карточки без плана: признак запроса плана и последнее действие.
fn activity_line(s: &Session, g: &str, inner: i64) -> String {
    let tag = match &s.ask {
        Some(a) if a.plan => {
            if a.state == AskState::Wait {
                c::amber("план запрошен")
            } else {
                c::green("запрос передан")
            }
        }
        _ => c::faint("без плана"),
    };
    let act = match s.activity() {
        Some((a, who)) => {
            let prefix = if who.is_empty() { String::new() } else { format!("{who} → ") };
            format!("{prefix}{}", a.describe())
        }
        None => String::new(),
    };
    let act_part = if act.is_empty() { String::new() } else { format!("  {}", c::dim(clip(&act, inner - 17))) };
    format!("{g}  {tag}{act_part}")
}

/// Третья строка карточки: папка, модель, контекст, токены, субагенты и память процесса.
fn meta_line(m: &Monitor, s: &Session, g: &str, inner: i64) -> String {
    let t = s.tokens();
    let run: Vec<&Agent> = s.running().collect();
    let mut models: Vec<String> = Vec::new();
    for a in &run {
        let short = short_model(&a.model);
        if !models.contains(&short) {
            models.push(short);
        }
    }
    let cwd = s.reg.as_ref().map(|r| r.cwd.as_str()).unwrap_or("");
    let ram = s
        .reg
        .as_ref()
        .and_then(|r| m.procs.get(r.pid))
        .filter(|place| place.mem != 0.0)
        .map(|place| format!("ОЗУ {}", mb(place.mem)))
        .unwrap_or_default();
    let parts = [
        basename(cwd),
        short_model(&s.model),
        if s.ctx != 0.0 { format!("контекст {}", tok(s.ctx)) } else { String::new() },
        format!("вых {} · вх {} · кэш {}", tok(t.o), tok(t.i), tok(t.c)),
        if run.is_empty() {
            String::new()
        } else {
            format!("агентов в работе {} ({})", run.len(), models.join(", "))
        },
        ram,
    ];
    let meta = parts.into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
    format!("{g}  {}", c::dim(clip(&meta, inner - 2)))
}

/// Экран списка: все видимые сессии карточками, при нехватке места — прокрутка к выбранной.
pub fn list_view(m: &Monitor, ui: &Ui, w: i64, h: i64, now: f64) -> Frame {
    let list: Vec<&Session> = m.visible(ui.sel.as_deref()).into_iter().map(|i| &m.sessions[i]).collect();
    let note =
        if list.is_empty() { "сессии Claude Code".to_string() } else { format!("сессий: {}", list.len()) };
    let mut body = vec![header(w, now, &note), String::new()];
    if list.is_empty() {
        body.push(format!("  {}", c::dim("Нет запущенных сессий Claude Code.")));
    }

    let selected_id = ui.sel.as_deref();
    let cards: Vec<[String; 3]> = list.iter().map(|s| card(m, s, w, now, Some(s.id.as_str()) == selected_id)).collect();
    // карточка занимает 4 строки вместе с пустой между карточками; шапка и подвал — три строки
    let fits = (h - 3).div_euclid(4).max(1);
    let sel_idx = list.iter().position(|s| Some(s.id.as_str()) == selected_id).unwrap_or(0) as i64;
    let total = cards.len() as i64;
    let from = (sel_idx - fits + 1).max(0).min((total - fits).max(0));
    for (k, card_lines) in cards.iter().skip(from as usize).take(fits as usize).enumerate() {
        if k > 0 {
            body.push(String::new());
        }
        body.extend(card_lines.iter().cloned());
    }
    let range = if total > fits {
        format!("  ·  {}–{} из {}", from + 1, (from + fits).min(total), total)
    } else {
        String::new()
    };
    let text = format!("↑↓ выбор · Enter открыть · p запросить план · q выход{range}");
    Frame { body, foot: footer(ui, &text, now) }
}
