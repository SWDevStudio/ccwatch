//! Советы по экономии токенов: что видно в логах сессии и сколько это примерно стоит,
//! отправленные советы с ответом агента и эффектом, скрипт «для вас» с подтверждением.

use std::cmp::Ordering;

use crate::app::Monitor;
use crate::clock;
use crate::fixes;
use crate::format::{ago, dur, tok};
use crate::requests::{AskState, MANUAL_TTL};
use crate::session::Session;
use crate::style::{self, bold, c};
use crate::text::{clip, spread, vis, wrap};
use crate::tips::{tip_title, ActiveTips, Kind, Tip};

use super::{footer, Frame, SetConfirm, Ui};

/// Пояснение в начале экрана: что за цены и откуда они.
const INTRO: &str = "Считаются по логам сессии, без запросов к модели. ≈ — цена в токенах обычного входа \
    (если чтение кэша ×1, то вход ×10, запись в кэш ×12,5–20, выход ×50): порядок величины и сколько это стоит сейчас, сэкономить удастся часть.";

/// Экран советов: список с выбором, отправленные советы, подтверждение скрипта.
pub fn tips_view(m: &Monitor, ui: &mut Ui, s: &Session, w: i64, h: i64, now: f64) -> Frame {
    let inner = w - 4;
    let st = s.status(now);
    let at = m.active_tips(s, now);

    // выбранный совет не выходит за список
    ui.tip_sel = ui.tip_sel.max(0).min((at.active.len() as i64 - 1).max(0));
    let sel = ui.tip_sel;

    let title = format!("Советы по экономии · {}", clip(&s.name(), inner - vis(&st.text) - 28));
    let left = format!("  {} {}", c::dim("←"), bold(c::text(title)));
    let mut body = vec![spread(&left, &format!("{} {} ", st.icon, st.text), w)];
    body.extend(wrap(INTRO, inner).into_iter().map(|l| format!("    {}", c::faint(l))));
    body.push(String::new());

    let pending: Vec<String> = match &s.ask {
        Some(a) if a.state == AskState::Wait => a.keys.clone(),
        _ => Vec::new(),
    };

    // скрипт совета ждёт подтверждения: что сделает по шагам и на что повлияет; всегда под заголовком
    let mut pre: Vec<String> = Vec::new();
    if let Some(sc) = &ui.set_confirm {
        if sc.id == s.id && sc.until > now {
            pre = set_confirm_lines(sc, inner);
        }
    }

    if !at.delivered.is_empty() {
        body.extend(delivered_lines(s, &at, inner, w, now));
    }

    if at.active.is_empty() {
        let text = if at.delivered.is_empty() {
            "Явных потерь не видно."
        } else {
            "Новых советов нет."
        };
        body.push(format!("    {}", c::dim(text)));
    }
    let mut sel_line = 0usize;
    for (k, t) in at.active.iter().enumerate() {
        let first_time_tip = t.kind == Kind::Time && (k == 0 || at.active[k - 1].kind != Kind::Time);
        if first_time_tip {
            body.push(format!(
                "    {}{}",
                bold(c::text("Скорость работы")),
                c::faint(" · цена — минуты работы агентов (у параллельных складываются)")
            ));
        }
        let is_sel = k as i64 == sel;
        if is_sel {
            sel_line = body.len();
        }
        body.extend(tip_lines(m, s, &at, t, &pending, is_sel, inner, w, now));
    }

    // если не влезает — сдвигаем так, чтобы выбранный совет был виден; шапка всегда на месте
    let rows = h - 2 - pre.len() as i64;
    let len = body.len() as i64;
    let start = if len > rows { (sel_line as i64 - 4).max(0).min(len - rows) } else { 0 };
    let mut shown = vec![body[0].clone()];
    shown.extend(pre);
    shown.extend(body.iter().skip((start + 1) as usize).cloned());

    // что сделает Enter на выбранном совете: агенту — отправить, «для вас» — запустить скрипт
    let cur = at.active.get(sel as usize);
    let enter = match cur {
        None => "",
        Some(t) if t.send => "Enter отправить совет · ",
        Some(t) if t.fix.is_some() => "Enter выполнить скрипт · ",
        Some(_) => "",
    };
    let u = match cur {
        Some(t) if t.send && t.fix.is_some() => "u выполнить скрипт · ",
        _ => "",
    };
    let text = format!("↑↓ выбор · {enter}{u}a отправить все агенту · Esc назад · q выход");
    Frame { body: shown, foot: footer(ui, &text, now) }
}

/// Скрипт совета: заголовок, шаги и подсказка «y — выполнить».
fn set_confirm_lines(sc: &SetConfirm, inner: i64) -> Vec<String> {
    let head = if sc.plan.undo {
        format!("Скрипт «вернуть как было»: {}", sc.fix.what)
    } else {
        format!("Скрипт: {} — {}", sc.fix.what, sc.fix.note)
    };
    let mut out: Vec<String> =
        wrap(&head, inner - 2).into_iter().map(|l| format!("    {}", bold(c::amber(l)))).collect();
    for step in fixes::steps(&sc.plan) {
        for (k, l) in wrap(&step, inner - 6).into_iter().enumerate() {
            let indent = if k > 0 { "   " } else { "" };
            out.push(format!("      {indent}{}", c::amber(l)));
        }
    }
    out.push(format!(
        "    {}",
        c::amber("y — выполнить, любая другая клавиша — отмена. Вернуть можно тем же скриптом на этом совете.")
    ));
    out.push(String::new());
    out
}

/// Блок «Отправленные советы»: заголовок, ответ агента, эффект «до / после» и общая мера.
fn delivered_lines(s: &Session, at: &ActiveTips, inner: i64, w: i64, now: f64) -> Vec<String> {
    let mut out = vec![format!("    {}", bold(c::text("Отправленные советы")))];
    let mut list: Vec<(&String, &f64)> = at.delivered.iter().collect();
    list.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(Ordering::Equal));
    for (key, t) in list {
        let effect = at.effects.get(key);
        let right = effect.map(|e| format!("{} ", style::fg(e.verdict.color, &e.verdict.text))).unwrap_or_default();
        let title = tip_title(key).map(str::to_string).unwrap_or_else(|| key.clone());
        let caption = format!("{}", c::faint(format!(" · доставлен {} назад", ago(now - t))));
        out.push(spread(&format!("    {}{caption}", c::text(title)), &right, w));

        match s.acks.get(key) {
            Some(ack) if ack.t >= t - 60e3 => {
                let head = if ack.ok {
                    c::green("ответ агента: принято")
                } else {
                    c::amber("ответ агента: не применимо")
                };
                let text = if ack.text.is_empty() { String::new() } else { format!(" — {}", ack.text) };
                out.push(format!("      {head}{}", c::dim(clip(&text, inner - 30))));
            }
            _ => {
                out.push(format!("      {}", c::faint("ответа агента пока нет: он появится в его следующем сообщении")))
            }
        }
        if let Some(e) = effect {
            let line = format!("{}: {} → {} (после: {})", e.what, e.fmt.apply(e.before), e.fmt.apply(e.after), e.n);
            out.push(format!("      {}", c::dim(clip(&line, inner - 4))));
            if let Some((before, after)) = e.overall {
                let overall = format!("в целом контекст на запрос: {} → {}", tok(before), tok(after));
                out.push(format!("      {}", c::faint(clip(&overall, inner - 4))));
            }
        }
    }
    out.push(format!(
        "    {}",
        c::faint(clip("Эффект — корреляция: мера может сдвинуться и от смены фазы задачи.", inner))
    ));
    out.push(String::new());
    out
}

/// Один совет: заголовок с отметками, цена справа и подробности с переносом.
#[allow(clippy::too_many_arguments)]
fn tip_lines(
    m: &Monitor,
    s: &Session,
    at: &ActiveTips,
    t: &Tip,
    pending: &[String],
    is_sel: bool,
    inner: i64,
    w: i64,
    now: f64,
) -> Vec<String> {
    let g = if is_sel { format!("  {} ", c::pink("▌")) } else { "    ".to_string() };
    let done = t.fix.as_ref().is_some_and(|f| m.settings.fix_done(f));
    let tag = tip_tag(s, at, t, pending, done, now);
    // у совета о программе (хук) цены нет: ни справа, ни в описании
    let price = match t.kind {
        Kind::Time => format!("≈{} ", dur(t.cost)),
        Kind::Tokens => format!("≈{} ", tok(t.w.unwrap_or(0.0))),
        Kind::Setup => String::new(),
    };
    let title = if is_sel { bold(c::text(&t.title)) } else { c::text(&t.title) };
    let mut out = vec![spread(&format!("{g}{title}{tag}"), &bold(c::cyan(price)), w)];

    let detail = match t.kind {
        Kind::Tokens => format!("{} {}. {}", tok(t.cost), t.unit, t.detail),
        Kind::Time | Kind::Setup => t.detail.clone(),
    };
    for line in wrap(&detail, inner - 4) {
        out.push(format!("{g}  {}", c::dim(line)));
    }
    out.push(String::new());
    out
}

/// Пометка у заголовка совета: для вас, отправлен, уже отправлялся, скрипт выполнен.
fn tip_tag(s: &Session, at: &ActiveTips, t: &Tip, pending: &[String], done: bool, now: f64) -> String {
    let mut tag = if !t.send {
        let text = if done {
            " · для вас"
        } else if t.fix.is_some() {
            " · для вас, есть скрипт"
        } else {
            " · для вас, вручную"
        };
        c::faint(text)
    } else if pending.contains(&t.key) {
        let until = s.ask.as_ref().map_or(0.0, |a| a.at + a.ttl.filter(|x| *x != 0.0).unwrap_or(MANUAL_TTL));
        c::green(format!(" · отправлен, ждёт действия агента (до {})", clock::clock(until)))
    } else if let Some(&since) = at.delivered.get(&t.key) {
        c::amber(format!(" · уже отправлялся {} назад, эффекта не видно", ago(now - since)))
    } else {
        String::new()
    };
    if done {
        tag += &c::green(" · скрипт выполнен, настройка стоит");
    }
    tag
}
