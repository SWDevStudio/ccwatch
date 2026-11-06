//! Экран одной сессии. Порядок на экране: шапка, план (середина), нижняя часть с последним действием,
//! агентами, советами, скиллами и таблицей токенов. Нижняя часть собирается раньше плана:
//! от её высоты зависит, сколько шагов плана влезет.

use std::cmp::Ordering;

use crate::app::Monitor;
use crate::clock;
use crate::format::{ago, approx, dur, js_round, num, tok};
use crate::session::{agent_time, entry_name, short_model, Agent, PlanSource, Session, Status, Step};
use crate::style::{bar, bold, c};
use crate::text::{clip, pad, pad_start, spread, vis, wrap};
use crate::tips::{ActiveTips, Kind, SkillCount, Tip};
use crate::transcript::{Tokens, PRICE};

use super::{footer, Frame, Ui};

/// Строка «Сейчас/Последнее» у метки: метка шириной 12.
fn label(t: &str) -> String {
    c::dim(pad(t, 12))
}

/// Экран сессии: шапка, середина с планом и подтверждениями, нижняя часть с токенами.
pub fn detail_view(m: &Monitor, ui: &Ui, s: &Session, w: i64, h: i64, now: f64) -> Frame {
    let inner = w - 4;
    let st = s.status(now);
    let head = head_lines(m, s, &st.icon, &st.text, w, inner);
    let tail = tail_lines(m, s, st.busy, inner, now);

    let p = s.progress();
    let ask = m.requests.ask_line(s, now);
    // ваше сообщение сессии: набор (клавиша m) или судьба последнего отправленного — место под него вычитаем из плана
    let typing_input = ui.input.as_ref().filter(|i| i.id == s.id);
    let typing = typing_input.is_some();
    let msg = if typing { None } else { m.requests.msg_line(s, now) };
    let extra = match (typing_input, &msg) {
        (Some(input), _) => wrap(&input.text, inner - 20).len() as i64 + 2,
        (None, Some(_)) => 2,
        (None, None) => 0,
    };

    let mut mid: Vec<String> = Vec::new();
    if p.n > 0 {
        mid = progress_lines(s, now, inner);
        let ask_rows = if ask.is_some() { 2 } else { 0 };
        let room = (h - 1 - head.len() as i64 - mid.len() as i64 - tail.len() as i64 - ask_rows - extra - 2).max(3);
        mid.extend(step_lines(s, room, inner, w, now));
        if let Some(a) = &ask {
            mid.push(String::new());
            mid.push(format!("    {a}"));
        }
    } else {
        let hint = if m.opts.auto_ask && m.requests.hook_installed {
            "План не ведётся. Если сессия поработает полминуты без плана, ccwatch попросит его сам; p — попросить сейчас."
        } else {
            "План не ведётся. Нажмите p, чтобы попросить сессию его составить."
        };
        mid.push(format!("    {}", ask.clone().unwrap_or_else(|| c::faint(hint))));
    }

    if let Some(msg) = &msg {
        mid.push(String::new());
        mid.push(format!("    {msg}"));
    }
    if let Some(input) = typing_input {
        let mut pre = input_lines(&input.text, inner);
        pre.append(&mut mid);
        mid = pre;
    }
    // подтверждение завершения сессии (клавиша x) — над всем, что в середине
    if let Some(kill) = &ui.confirm {
        if kill.id == s.id && kill.until > now {
            let mut pre = kill_confirm_lines(s, st.busy, inner);
            pre.append(&mut mid);
            mid = pre;
        }
    }

    let kill = if s.reg.as_ref().map(|r| r.entrypoint.as_str()) == Some("claude-desktop") {
        ""
    } else {
        "x завершить · "
    };
    let keys = if typing {
        "Enter отправить сообщение · Esc отмена".to_string()
    } else {
        format!(
            "Esc назад · Enter весь план · s советы · i скиллы · b идеи · p запросить план · m написать · {kill}q выход"
        )
    };

    let mut body = head;
    body.append(&mut mid);
    body.extend(tail);
    Frame { body, foot: footer(ui, &keys, now) }
}

/// Шапка: имя и статус, точка входа и модель, место запуска (окно и память).
fn head_lines(m: &Monitor, s: &Session, icon: &str, status_text: &str, w: i64, inner: i64) -> Vec<String> {
    let r = s.reg.clone().unwrap_or_default();
    let name = clip(&s.name(), inner - vis(status_text) - 6);
    let left = format!("  {} {}", c::dim("←"), bold(c::text(name)));
    let right = format!("{icon} {status_text} ");
    let plan_source = if s.plan.is_empty() {
        ""
    } else {
        match s.plan_source {
            PlanSource::Text => "план из меток в ответах",
            PlanSource::File => "план из файла ~/.ccwatch/plans",
            _ => "",
        }
    };
    let facts = [
        r.cwd.clone(),
        s.model.clone(),
        entry_name(&r.entrypoint),
        s.id.chars().take(8).collect(),
        plan_source.to_string(),
    ];
    let mut head =
        vec![spread(&left, &right, w), format!("    {}", c::dim(clip(&join_nonempty(&facts, " · "), inner)))];
    let at = m.procs.where_line(s);
    if !at.is_empty() {
        for (k, l) in wrap(&at, inner - 11).into_iter().enumerate() {
            let lead = if k > 0 { " ".repeat(10) } else { c::dim("Запущена: ") };
            head.push(format!("    {lead}{}", c::text(l)));
        }
    }
    head.push(String::new());
    head
}

/// Нижняя часть: последнее действие, агенты, советы, скиллы и таблица токенов с памяткой цен.
fn tail_lines(m: &Monitor, s: &Session, busy: bool, inner: i64, now: f64) -> Vec<String> {
    let mut tail = vec![String::new()];
    if let Some(last) = &s.last {
        let when = c::faint(format!("{} назад", ago(now - last.at)));
        let caption = if busy { "Сейчас" } else { "Последнее" };
        let text_room = inner - 14 - vis(&when);
        tail.push(format!("    {}{}  {when}", label(caption), c::text(clip(&last.describe(), text_room))));
    }
    agents_lines(s, inner, now, &mut tail);
    tips_line(m, s, now, &mut tail);
    skills_line(s, inner, &mut tail);
    ideas_line(s, &mut tail);
    tail.push(String::new());
    tail.extend(token_lines(s, inner, now));
    tail
}

/// Блок «Агенты»: сколько работает и сколько всего, затем до пяти работающих.
fn agents_lines(s: &Session, inner: i64, now: f64, tail: &mut Vec<String>) {
    if s.agents.is_empty() {
        return;
    }
    let run: Vec<&Agent> = s.running().collect();
    tail.push(format!(
        "    {}{}",
        label("Агенты"),
        c::text(format!("{} в работе · {} всего", run.len(), s.agents.len()))
    ));
    for a in run.iter().take(5) {
        let when = match &a.t.last {
            Some(last) => c::faint(format!("{} назад", ago(now - last.at))),
            None => String::new(),
        };
        let mut info = format!("{} · {} · {}", a.name, short_model(&a.model), ago(agent_time(a, now)));
        if let Some(last) = &a.t.last {
            info += &format!(" → {}", last.describe());
        }
        let info_room = inner - 16 - vis(&when);
        let when_part = if when.is_empty() { String::new() } else { format!("  {when}") };
        tail.push(format!("    {}{}{}{when_part}", " ".repeat(12), c::cyan("◆ "), c::dim(clip(&info, info_room))));
    }
    if run.len() > 5 {
        tail.push(format!("    {}{}", " ".repeat(12), c::faint(format!("… и ещё {}", run.len() - 5))));
    }
}

/// Строка «Советы»: сколько новых и первый из них с ценой, сколько уже отправлено.
fn tips_line(m: &Monitor, s: &Session, now: f64, tail: &mut Vec<String>) {
    let at: ActiveTips = m.active_tips(s, now);
    if at.active.is_empty() && at.delivered.is_empty() {
        return;
    }
    // у совета о программе (хук) цены нет
    let price = |t: &Tip| match t.kind {
        Kind::Time => format!(" ≈{}", dur(t.cost)),
        Kind::Tokens => format!(" ≈{}", tok(t.w.unwrap_or(0.0))),
        Kind::Setup => String::new(),
    };
    let head = match at.active.first() {
        Some(first) => {
            format!("{} новых, первый — «{}»{}", at.active.len(), clip(&first.title, 40), price(first))
        }
        None => "новых нет".to_string(),
    };
    let sent =
        if at.delivered.is_empty() { String::new() } else { format!(", отправлено {}", at.delivered.len()) };
    tail.push(format!("    {}{}{}", label("Советы"), c::amber(format!("{head}{sent}")), c::faint(" · s — открыть")));
}

/// Строка «Скиллы»: сколько вызвали агенты и вы, и три самых частых.
fn skills_line(s: &Session, inner: i64, tail: &mut Vec<String>) {
    let sk = s.skill_use();
    if sk.by_skill.is_empty() {
        return;
    }
    let mut sorted: Vec<(&String, &SkillCount)> = sk.by_skill.iter().collect();
    sorted.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    let top = sorted.iter().take(3).map(|(name, r)| format!("{name} ×{}", r.n)).collect::<Vec<_>>().join(", ");
    let summary = format!("агенты {}, вы {}: {top}", sk.agent, sk.user_skills);
    tail.push(format!("    {}{}{}", label("Скиллы"), c::text(clip(&summary, inner - 30)), c::faint(" · i — история")));
}

/// Строка «Идеи»: сколько идей агент предложил для анализатора; нет идей — строки нет.
fn ideas_line(s: &Session, tail: &mut Vec<String>) {
    if s.ideas.is_empty() {
        return;
    }
    let count = c::text(format!("{} от агента", s.ideas.len()));
    tail.push(format!("    {}{count}{}", label("Идеи"), c::faint(" · b — открыть")));
}

/// Сумма строк токенов: сколько агентов и сколько времени, если они есть хоть у одной строки.
#[derive(Default)]
struct Sum {
    t: Tokens,
    agents: Option<usize>,
    ms: Option<f64>,
}

/// Таблица токенов: главный агент, субагенты по моделям, итог, контекст и памятка цен.
fn token_lines(s: &Session, inner: i64, now: f64) -> Vec<String> {
    let rows = s.rows(now);
    let mut total = Sum::default();
    for r in &rows {
        total.t.add(&r.t);
        total.agents = match r.agents {
            None => total.agents,
            Some(a) => Some(total.agents.unwrap_or(0) + a),
        };
        total.ms = match r.ms {
            None => total.ms,
            Some(ms) => Some(total.ms.unwrap_or(0.0) + ms),
        };
    }
    // ширина колонки подписей: от 12 до 28 символов, плюс два пробела
    let widest = rows.iter().map(|r| vis(&r.label)).fold(12, i64::max).min(28);
    let lw = widest + 2;
    let all = total.t.cost();
    let part = |v: f64| -> Option<String> {
        if all == 0.0 {
            None
        } else {
            Some(format!("{}%", num(js_round(v / all * 100.0))))
        }
    };

    let heads = ["выход", "вход", "из кэша", "доля", "агентов", "время"];
    let header: String = heads.iter().map(|x| pad_start(x, 9)).collect();
    let mut out = vec![format!("    {}", c::faint(format!("{}{header}", pad("Токены", lw))))];
    for r in &rows {
        out.push(format!("    {}{}", c::dim(pad(&r.label, lw)), c::text(token_cells(&r.t, r.agents, r.ms, &part))));
    }
    if rows.len() > 1 {
        out.push(format!(
            "    {}{}",
            c::dim(pad("итого", lw)),
            bold(c::cyan(token_cells(&total.t, total.agents, total.ms, &part)))
        ));
    }
    out.push(format!("    {}{}", c::dim(pad("контекст", lw)), c::text(cell(Some(tok(s.ctx))))));
    if all != 0.0 {
        out.extend(memo_lines(&total.t, inner, &part));
    }
    out
}

/// Ячейка таблицы: выровнена вправо, пусто — «—».
fn cell(v: Option<String>) -> String {
    pad_start(&v.unwrap_or_else(|| "—".to_string()), 9)
}

/// Одна строка таблицы: выход, вход, из кэша, доля в цене, агенты и время.
fn token_cells(t: &Tokens, agents: Option<usize>, ms: Option<f64>, part: &dyn Fn(f64) -> Option<String>) -> String {
    [
        cell(Some(tok(t.o))),
        cell(Some(tok(t.i))),
        cell(Some(tok(t.c))),
        cell(part(t.cost())),
        cell(agents.map(|a| a.to_string())),
        cell(ms.map(ago)),
    ]
    .concat()
}

/// Часть цены сессии для памятки: название, сумма в обычных токенах и совет, чего бояться.
struct MemoPart {
    name: &'static str,
    v: f64,
    why: &'static str,
}

/// Памятка: цена токена относительно чтения кэша и разбивка цены по частям.
fn memo_lines(t: &Tokens, inner: i64, part: &dyn Fn(f64) -> Option<String>) -> Vec<String> {
    let parts = [
        MemoPart {
            name: "чтение кэша",
            v: t.c * PRICE.read,
            why: "за токен оно самое дешёвое, но каждый запрос перечитывает весь контекст — бояться стоит его размера",
        },
        MemoPart {
            name: "вход и запись в кэш",
            v: t.wi,
            why: "бояться стоит больших новых входов и перезаписи кэша после пауз",
        },
        MemoPart {
            name: "выход", v: t.o * PRICE.output, why: "бояться стоит длинных ответов и размышлений"
        },
    ];
    let mut by_value: Vec<&MemoPart> = parts.iter().collect();
    by_value.sort_by(|a, b| b.v.partial_cmp(&a.v).unwrap_or(Ordering::Equal));
    let top = by_value[0];

    // цена токена в единицах чтения кэша, одна цифра после запятой
    let k = |v: f64| format!("×{}", num(js_round(v / PRICE.read * 10.0) / 10.0).replace('.', ","));
    let rates = format!(
        "Цена токена, если чтение кэша ×1: вход {} · запись в кэш {} (на час {}) · выход {}",
        k(PRICE.input),
        k(PRICE.write_5m),
        k(PRICE.write_1h),
        k(PRICE.output)
    );
    let shares =
        parts.iter().map(|p| format!("{} {}", p.name, part(p.v).unwrap_or_default())).collect::<Vec<_>>().join(" · ");
    let memo = format!("Здесь цена делится так: {shares}. Больше всего — {}: {}.", top.name, top.why);

    let mut out = vec![String::new()];
    out.extend(wrap(&rates, inner - 2).into_iter().map(|l| format!("    {}", c::text(l))));
    out.extend(wrap(&memo, inner - 2).into_iter().map(|l| format!("    {}", c::faint(l))));
    out
}

/// Шкала плана и строки оценки времени: итог, сколько прошло и осталось, когда закончит.
fn progress_lines(s: &Session, now: f64, inner: i64) -> Vec<String> {
    let p = s.progress();
    let mut out = Vec::new();
    let pct = js_round(p.done as f64 / p.n as f64 * 100.0);
    let lbl = match p.cur {
        Some(_) => format!("шаг {} из {}", p.idx, p.n),
        None if p.done == p.n => "готово".to_string(),
        None => format!("{} из {}", p.done, p.n),
    };
    let tail_text = format!("  {}%   {lbl}", pad_start(&num(pct), 3));
    let bar_w = (inner - vis(&tail_text) - 2).min(60).max(10);
    out.push(format!("    {}{}", bar(p.done as f64 / p.n as f64, bar_w), c::text(&tail_text)));

    match s.estimate(now) {
        Some(e) => {
            let mut parts = if e.all_done {
                vec![format!("оценка {}", approx(e.total)), format!("заняло {}", dur(e.spent))]
            } else {
                vec![
                    format!("оценка {}", approx(e.total)),
                    format!("прошло {}", dur(e.spent)),
                    format!("осталось {}", approx(e.left)),
                    format!("закончит ≈ {}", clock::clock(now + e.left)),
                ]
            };
            if e.missing > 0 {
                parts.push(format!("без оценки шагов: {}", e.missing));
            }
            out.push(format!("    {}", c::cyan(parts.join(" · "))));
            if let (Some(over), false) = (&e.over, e.all_done) {
                let txt = format!(
                    "Шаг «{}» идёт {} при оценке {}",
                    clip(&over.content, 40),
                    dur(over.spent),
                    approx(over.est)
                );
                out.push(format!("    {}", c::amber(txt)));
            }
        }
        None => out.push(format!("    {}", c::faint("Оценок времени у шагов нет."))),
    }
    out.push(String::new());
    out
}

/// Строки шагов плана вокруг текущего: по возможности целиком, иначе окно с отметками «ещё выше/ниже».
fn step_lines(s: &Session, room: i64, inner: i64, w: i64, now: f64) -> Vec<String> {
    let p = s.progress();
    let steps = &s.plan;
    let n = steps.len() as i64;
    let focus = p.cur.map(|k| k as i64).unwrap_or(p.done as i64 - 1);
    let from = if n > room { (focus - room.div_euclid(2)).max(0).min(n - room) } else { 0 };

    let mut out = Vec::new();
    if from > 0 {
        out.push(format!("    {}", c::faint(format!("… ещё {from} выше"))));
    }
    for x in steps.iter().skip(from as usize).take(room as usize) {
        let right = step_time(x, now);
        let txt = clip(&x.content, inner - 6 - vis(&right));
        let left = format!("    {}  {}", step_mark(x), step_text(x, &txt));
        out.push(if right.is_empty() { left } else { spread(&left, &format!("{right}  "), w) });
    }
    let below = n - from - room;
    if below > 0 || from > 0 {
        let more = if below > 0 { format!("… ещё {below} ниже · ") } else { String::new() };
        out.push(format!("    {}", c::faint(format!("{more}Enter — весь план"))));
    }
    out
}

/// Строки набора сообщения сессии: первая с подписью, курсор ▌ в конце; подсказка и пустая строка под ним.
fn input_lines(text: &str, inner: i64) -> Vec<String> {
    let mut lines = wrap(text, inner - 20);
    // wrap съедает пробел, а курсор должен стоять после него
    if text.ends_with(' ') {
        if let Some(last) = lines.last_mut() {
            last.push(' ');
        }
    }
    let n = lines.len();
    let mut out: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(k, l)| {
            let lead = if k > 0 { " ".repeat(18) } else { c::cyan("Сообщение сессии: ") };
            let cursor = if k == n - 1 { c::pink("▌") } else { String::new() };
            format!("    {lead}{}{cursor}", c::text(l))
        })
        .collect();
    out.push(format!(
        "    {}",
        c::faint("Enter — отправить · Esc — отмена. Агент получит его при следующем вызове инструмента.")
    ));
    out.push(String::new());
    out
}

/// Подтверждение завершения сессии: что случится и как вернуть.
fn kill_confirm_lines(s: &Session, busy: bool, inner: i64) -> Vec<String> {
    let warn =
        if busy { " Она сейчас работает — текущий шаг оборвётся." } else { "" };
    let text = format!(
        "Завершить сессию {}? Процесс claude закроется вместе с дочерними, транскрипт останется: \
         вернуть можно командой claude --resume {}.{warn} y — да, любая другая клавиша — нет.",
        s.name(),
        s.id
    );
    let mut out: Vec<String> = wrap(&text, inner - 2).into_iter().map(|l| format!("    {}", c::amber(l))).collect();
    out.push(String::new());
    out
}

/// Знак шага: ✓ сделан, ▶ в работе, · ждёт.
pub(super) fn step_mark(x: &Step) -> String {
    match &x.status {
        Status::Completed => c::green("✓"),
        Status::InProgress => c::pink("▶"),
        _ => c::faint("·"),
    }
}

/// Текст шага: приглушённый у сделанного, яркий розовый у текущего.
pub(super) fn step_text(x: &Step, t: &str) -> String {
    match &x.status {
        Status::Completed => c::dim(t),
        Status::InProgress => bold(c::pink(t)),
        _ => c::text(t),
    }
}

/// Справа у шага: факт и оценка у сделанного, сколько идёт у текущего, оценка у будущих.
pub(super) fn step_time(x: &Step, now: f64) -> String {
    match &x.status {
        Status::Completed => {
            let fact = if x.done_at != 0.0 && x.started_at != 0.0 && x.done_at > x.started_at {
                dur(x.done_at - x.started_at)
            } else {
                String::new()
            };
            let est = if x.est != 0.0 { approx(x.est) } else { String::new() };
            c::dim(join_nonempty(&[fact, est], " / "))
        }
        Status::InProgress => {
            let spent = if x.started_at != 0.0 { now - x.started_at } else { 0.0 };
            let spent_text = if spent != 0.0 { format!("идёт {}", dur(spent)) } else { String::new() };
            let est = if x.est != 0.0 { approx(x.est) } else { String::new() };
            let txt = join_nonempty(&[spent_text, est], " / ");
            if x.est != 0.0 && spent > x.est {
                c::amber(txt)
            } else {
                c::pink(txt)
            }
        }
        _ => {
            if x.est != 0.0 {
                c::faint(approx(x.est))
            } else {
                String::new()
            }
        }
    }
}

/// Непустые части через разделитель.
fn join_nonempty(parts: &[String], sep: &str) -> String {
    parts.iter().filter(|p| !p.is_empty()).map(String::as_str).collect::<Vec<_>>().join(sep)
}
