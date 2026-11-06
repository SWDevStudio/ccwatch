//! Советы по скорости: где агенты теряют время — долгие проверки, мелкие команды, ожидание в цикле,
//! повторные прогоны проверок, упор в тайм-аут, долгий разгон субагентов и размышления модели.

use std::cmp::Ordering;
use std::collections::HashMap;

use indexmap::IndexMap;
use serde_json::json;

use super::{lead, Kind, Tip, TAIL};
use crate::clock;
use crate::fixes::{Change, Fix};
use crate::format::{approx, dur, pct, plural, times, to_fixed};
use crate::session::{agent_time, short_model, Agent, Session};
use crate::text::clip;
use crate::transcript::commands::{is_check_role, is_trivial};
use crate::transcript::{Call, Transcript, RAMP_OK, SHELL_LIMIT};

/// Прогон проверки одного агента. owner: 0 — главный агент, 1.. — субагенты.
pub struct CheckRun {
    pub owner: usize,
    pub t: f64,
    pub dt: f64,
    pub call: Call,
    /// Тот же набор уже проверен на этом же коде.
    pub repeat: bool,
    /// Повтор на коммите, который уже проверил другой агент.
    pub by_other: bool,
}

/// Команда упёрлась в тайм-аут: шла до своего лимита (по умолчанию 2 мин, больше 10 мин не ставится)
/// или так сказано в выводе.
pub fn hit_limit(c: &Call, dt: f64) -> bool {
    // limit 0 или NaN в JS — «не задан», берётся лимит оболочки по умолчанию
    let limit = if c.limit != 0.0 && !c.limit.is_nan() { c.limit } else { SHELL_LIMIT };
    !c.bg && (c.timed_out || dt >= limit.min(600e3) - 5e3)
}

/// Группа однотипных команд: сколько раз запускали и сколько заняли.
struct Group {
    label: String,
    n: i64,
    ms: f64,
}

/// Счётчик команд по времени: сколько и сколько миллисекунд.
#[derive(Default)]
struct Tally {
    n: i64,
    ms: f64,
}

/// Упор в тайм-аут: сколько команд, сколько времени, сколько из них — циклы ожидания и до предела 10 минут.
#[derive(Default)]
struct Limit {
    n: i64,
    ms: f64,
    poll: i64,
    max: i64,
}

fn desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// Истинность числа как в JS: 0 и NaN — ложь.
pub(super) fn truthy(x: f64) -> bool {
    x != 0.0 && !x.is_nan()
}

impl Session {
    /// Советы по скорости, по убыванию цены (минуты работы агентов).
    pub(super) fn speed_tips(&self) -> Vec<Tip> {
        let main = match &self.main {
            Some(m) => m,
            None => return Vec::new(),
        };
        let mut owners: Vec<&Transcript> = vec![main];
        owners.extend(self.agents.iter().map(|a| &a.t));

        let mut slow: IndexMap<String, Group> = IndexMap::new();
        let mut small = Tally::default();
        let mut poll = Tally::default();
        let mut think = 0.0;
        let mut model = 0.0;
        let mut reqs: i64 = 0;
        let mut multi: i64 = 0;
        // «модель effort» → мс размышлений у всех агентов
        let mut think_by: IndexMap<String, f64> = IndexMap::new();
        let mut limit = Limit::default();
        for t in &owners {
            think += t.think_ms;
            for (k, ms) in &t.think_by {
                *think_by.entry(k.clone()).or_insert(0.0) += ms;
            }
            model += t.model_ms();
            for r in &t.reqs {
                if r.tools > 0 {
                    reqs += 1;
                    if r.tools > 1 {
                        multi += 1;
                    }
                }
            }
            for c in &t.calls {
                let at = match t.result_at.get(&c.id) {
                    Some(&at) if truthy(at) => at,
                    _ => continue,
                };
                if !c.shell {
                    continue;
                }
                let dt = (at - c.t).max(0.0);
                if hit_limit(c, dt) {
                    limit.n += 1;
                    limit.ms += dt;
                    if c.poll {
                        limit.poll += 1;
                    }
                    if dt >= 590e3 {
                        limit.max += 1;
                    }
                }
                if c.poll {
                    poll.n += 1;
                    poll.ms += dt;
                    continue;
                }
                if is_trivial(&c.prog) {
                    small.n += 1;
                    small.ms += dt;
                    continue;
                }
                if !c.key.starts_with("cmd:") {
                    continue;
                }
                let g =
                    slow.entry(c.key.clone()).or_insert_with(|| Group { label: c.key[4..].to_string(), n: 0, ms: 0.0 });
                g.n += 1;
                g.ms += dt;
            }
        }
        let mut out = Vec::new();
        let tail = TAIL;

        let mut top: Vec<&Group> = slow.values().filter(|g| g.n >= 3 && g.ms / g.n as f64 >= 60e3).collect();
        top.sort_by(|a, b| desc(a.ms, b.ms));
        top.truncate(3);
        let slow_ms: f64 = top.iter().map(|g| g.ms).sum();
        if slow_ms >= 10.0 * 60e3 {
            let items: Vec<String> = top
                .iter()
                .map(|g| {
                    format!(
                        "«{}» — {} {}, в среднем {}, всего {}",
                        clip(&g.label, 50),
                        g.n,
                        plural(g.n, "запуск", "запуска", "запусков"),
                        dur(g.ms / g.n as f64),
                        dur(g.ms)
                    )
                })
                .collect();
            let joined = items.join("; ");
            out.push(Tip {
                key: "slowcheck".into(),
                kind: Kind::Time,
                title: "Долгие проверки".into(),
                cost: slow_ms,
                unit: "",
                send: true,
                w: None,
                detail: joined.clone(),
                text: lead("slowcheck", "скорости работы")
                    + &format!("долгие проверки отнимают много времени: {joined}. По ходу работы гоняй точечные проверки, ")
                    + "полный прогон — один раз в конце; не повторяй прогон, который уже прошёл, если код не менялся; независимые наборы "
                    + "и сиды запускай параллельно (в фоне или пачкой в одном скрипте)."
                    + tail,
                fix: None,
            });
        }
        if small.n >= 50 && small.ms >= 10.0 * 60e3 {
            let avg = to_fixed(small.ms / small.n as f64 / 1000.0, 1);
            let share = pct(multi as f64 / (reqs.max(1)) as f64);
            out.push(Tip {
                key: "smallshell".into(),
                kind: Kind::Time,
                title: "Мелкие команды оболочки".into(),
                cost: small.ms,
                unit: "",
                send: true,
                w: None,
                detail: format!(
                    "{} вызовов cat, grep, sed, ls и подобных, в среднем {avg} с каждый; несколько инструментов сразу вызывают только {share} запросов.",
                    small.n
                ),
                text: lead("smallshell", "скорости работы")
                    + &format!("мелкие команды оболочки (cat, grep, sed, ls и подобные) — {} вызовов, в среднем {avg} с каждый. ", small.n)
                    + "Читай и ищи встроенными Read, Grep и Glob, а независимые чтения и поиски запрашивай одним сообщением сразу."
                    + tail,
                fix: None,
            });
        }
        if poll.ms >= 5.0 * 60e3 {
            out.push(Tip {
                key: "polling".into(),
                kind: Kind::Time,
                title: "Ожидание в цикле".into(),
                cost: poll.ms,
                unit: "",
                send: true,
                w: None,
                detail: format!(
                    "{} {} с циклом until/while/for и sleep, всего {}.",
                    poll.n,
                    plural(poll.n, "команда", "команды", "команд"),
                    dur(poll.ms)
                ),
                text: lead("polling", "скорости работы")
                    + &format!(
                        "агенты ждут в цикле (until/while/for со sleep): {}, всего {}. Долгие команды запускай в фоне ",
                        times(poll.n),
                        dur(poll.ms)
                    )
                    + "и занимайся другим, результат придёт уведомлением; не опрашивай файл в цикле."
                    + tail,
                fix: None,
            });
        }

        // Повторная полная проверка: тот же набор на коде, который уже проверяли (тот же коммит или правок не было)
        let runs = self.check_runs();
        let again: Vec<&CheckRun> = runs.iter().filter(|r| r.repeat).collect();
        let again_ms: f64 = again.iter().map(|r| r.dt).sum();
        if again.len() >= 5 && again_ms >= 10.0 * 60e3 {
            let mut by_suite: IndexMap<String, Tally> = IndexMap::new();
            for r in &again {
                let g = by_suite.entry(r.call.suite.clone()).or_default();
                g.n += 1;
                g.ms += r.dt;
            }
            let mut suites: Vec<(&String, &Tally)> = by_suite.iter().collect();
            suites.sort_by(|a, b| desc(a.1.ms, b.1.ms));
            let items: Vec<String> = suites
                .iter()
                .take(3)
                .map(|(k, g)| format!("«{}» — {}, {}", clip(k, 50), times(g.n), dur(g.ms)))
                .collect();
            let other = again.iter().filter(|r| r.by_other).count();
            let roles = self.check_share(clock::now());
            let mut parts = vec![format!(
                "{} из {} {} проверок повторили набор на уже проверенном коде, всего {}{}.",
                again.len(),
                runs.len(),
                plural(runs.len() as i64, "прогона", "прогонов", "прогонов"),
                dur(again_ms),
                if other > 0 {
                    format!("; {other} — на коммите, который уже проверил другой агент")
                } else {
                    String::new()
                }
            )];
            parts.push(format!("Чаще всего: {}.", items.join("; ")));
            if truthy(roles) {
                parts.push(format!(
                    "На проверяющих агентов (тесты, проверки, ревью — по типу агента или заданию) ушло {} времени субагентов.",
                    pct(roles)
                ));
            }
            let joined = items.join("; ");
            out.push(Tip {
                key: "recheck".into(),
                kind: Kind::Time,
                title: "Повторные проверки на том же коде".into(),
                cost: again_ms,
                unit: "",
                send: true,
                w: None,
                detail: parts.join(" "),
                text: lead("recheck", "скорости работы")
                    + &format!(
                        "одни и те же наборы проверок прогоняются повторно на коде, который уже проверен: {} {}, ",
                        again.len(),
                        plural(again.len() as i64, "прогон", "прогона", "прогонов")
                    )
                    + &format!("всего {} ({joined}). Перед полным прогоном смотри, не проверен ли уже этот коммит (лог или отметка с хешем); ", dur(again_ms))
                    + "не запускай прошедший набор снова, если код не менялся; не поручай один и тот же полный набор и исполнителю, и проверяющему — "
                    + "проверяющий берёт готовый лог исполнителя и сам гоняет только то, чего там нет."
                    + tail,
                fix: None,
            });
        }

        // Упор в тайм-аут: команда шла до лимита инструмента — чаще всего это ожидание в цикле или зависший прогон
        if limit.n >= 3 && limit.ms >= 15.0 * 60e3 {
            let loops = if limit.poll > 0 {
                format!(" Циклов ожидания среди них — {}.", limit.poll)
            } else {
                String::new()
            };
            let max = if limit.max > 0 {
                format!(
                    ": {} — до предела в 10 минут, остальные — до 2 минут по умолчанию или заданного тайм-аута",
                    limit.max
                )
            } else {
                String::new()
            };
            out.push(Tip {
                key: "timeout".into(),
                kind: Kind::Time,
                title: "Команды упираются в тайм-аут".into(),
                cost: limit.ms,
                unit: "",
                send: true,
                w: None,
                detail: format!(
                    "{} {} до своего тайм-аута, всего {}{}.{} ",
                    limit.n,
                    plural(limit.n, "команда шла", "команды шли", "команд шли"),
                    dur(limit.ms),
                    max,
                    loops
                ) + "Всё это время агент стоит и ждёт.",
                text: lead("timeout", "скорости работы")
                    + &format!(
                        "команды упираются в тайм-аут инструмента: {} {}, всего {}{}. ",
                        limit.n,
                        plural(limit.n, "вызов", "вызова", "вызовов"),
                        dur(limit.ms),
                        loops
                    )
                    + "Долгие прогоны и ожидание файла или процесса запускай в фоне (run_in_background) и жди уведомления, а не держи вызов до лимита; "
                    + "если ждёшь в команде — ставь короткий явный тайм-аут (timeout 120 …) и печатай понятный итог при неудаче."
                    + tail,
                fix: None,
            });
        }

        // Долгий разгон: от задания субагенту до его первой правки кода (Write/Edit), без перерывов
        let ramps: Vec<&Agent> = self.agents.iter().filter(|a| a.t.ramp_ms >= 0.0).collect();
        let ramp_ms: f64 = ramps.iter().map(|a| (a.t.ramp_ms - RAMP_OK).max(0.0)).sum();
        if ramps.len() >= 3 && ramp_ms >= 20.0 * 60e3 {
            // у каждой модели — среднее время разгона
            let mut by_model: IndexMap<String, Tally> = IndexMap::new();
            for a in &ramps {
                let m = short_model(if a.t.model.is_empty() { &a.model } else { &a.t.model });
                let g = by_model.entry(m).or_default();
                g.n += 1;
                g.ms += a.t.ramp_ms;
            }
            let mut models: Vec<(&String, &Tally)> = by_model.iter().collect();
            models.sort_by(|a, b| desc(a.1.ms / a.1.n as f64, b.1.ms / b.1.n as f64));
            let items: Vec<String> = models
                .iter()
                .map(|(m, g)| {
                    format!(
                        "у {m} в среднем {} ({} {})",
                        approx(g.ms / g.n as f64),
                        g.n,
                        plural(g.n, "агент", "агента", "агентов")
                    )
                })
                .collect();
            let now = clock::now();
            let all: f64 = ramps.iter().map(|a| a.t.ramp_ms).sum();
            let work: f64 = ramps.iter().map(|a| agent_time(a, now)).sum();
            let share = if truthy(work) {
                format!(" Это {} времени агентов, которые правили код.", pct(all / work))
            } else {
                String::new()
            };
            let joined = items.join("; ");
            out.push(Tip {
                key: "ramp".into(),
                kind: Kind::Time,
                title: "Субагенты долго разгоняются".into(),
                cost: ramp_ms,
                unit: "",
                send: true,
                w: None,
                detail: format!(
                    "До первой правки кода: {joined}.{share} Цена — время сверх первых {} на каждого.",
                    dur(RAMP_OK)
                ),
                text: lead("ramp", "скорости работы")
                    + &format!("субагенты долго разгоняются: до первой правки кода {joined}.{share} ")
                    + "В задании исполнителю сразу давай файлы и места правки, нужные выжимки и критерий готовности, чтобы он не изучал проект заново; "
                    + "если нужно исследование — сделай его одним коротким агентом и передай исполнителям итог."
                    + tail,
                fix: None,
            });
        }

        if think >= 20.0 * 60e3 && truthy(model) && think >= 0.4 * model {
            // больше всех размышляет одна пара «модель + effort» — её и предлагаем снизить на ступень
            let mut by_think: Vec<(&String, &f64)> = think_by.iter().collect();
            by_think.sort_by(|a, b| desc(*a.1, *b.1));
            let (top_key, top_ms) = match by_think.first() {
                Some((k, ms)) => (k.as_str(), **ms),
                None => ("", 0.0),
            };
            // «модель effort» делим по пробелу, как split(' ') в JS: effort может отсутствовать
            let mut parts = top_key.split(' ');
            let tm = parts.next().unwrap_or("");
            let te: Option<&str> = parts.next();
            let target = match te {
                Some("max") | Some("xhigh") => Some("high"),
                Some("high") => Some("medium"),
                _ => None,
            };
            // в JS отсутствующий effort печатается как «undefined»
            let te_text = te.unwrap_or("undefined");
            let mut tip = Tip {
                key: "thinking".into(),
                kind: Kind::Time,
                title: format!("Размышления — {} времени модели", pct(think / model)),
                cost: think,
                unit: "",
                send: target.is_some(),
                w: None,
                detail: format!(
                    "{} из {} работы модели ушло на размышления, из них {} — {} на effort {}.",
                    dur(think),
                    dur(model),
                    dur(top_ms),
                    short_model(tm),
                    te_text
                ),
                text: String::new(),
                fix: None,
            };
            if let Some(target) = target {
                // два способа: попросить эту сессию (effort в вызове Agent) или поменять настройку для всех сессий
                tip.detail += &format!(
                    " Исполнителям обычно хватает effort {target}. Enter — попросить эту сессию запускать исполнителей с effort {target}"
                );
                tip.text = lead("thinking", "скорости работы")
                    + &format!(
                        "размышления заняли {} времени модели ({} из {}), ",
                        pct(think / model),
                        dur(think),
                        dur(model)
                    )
                    + &format!(
                        "больше всего у {} на effort {te_text}. Пользователь просит запускать субагентов-исполнителей (правка кода, проверки, документы) ",
                        short_model(tm)
                    )
                    + &format!(
                        "с effort {target}: передавай effort: \"{target}\" в вызове Agent. Effort {te_text} оставляй для задач, где нужен глубокий анализ: "
                    )
                    + "архитектура, сложная отладка."
                    + tail;
                if tm.starts_with("claude-") {
                    tip.fix = Some(Fix {
                        what: format!("effort {target} для {}", short_model(tm)),
                        note: format!("для всех сессий и субагентов на {}, в том числе ваших", short_model(tm)),
                        changes: vec![Change {
                            path: vec!["modelSettings".into(), tm.into(), "effortLevel".into()],
                            to: json!(target),
                            label: None,
                        }],
                    });
                    tip.detail += &format!(
                        "; u — скриптом поставить {target} для {} в ~/.claude/settings.json (для всех сессий).",
                        short_model(tm)
                    );
                } else {
                    tip.detail += ".";
                }
            }
            out.push(tip);
        }
        out.sort_by(|a, b| desc(a.cost, b.cost));
        out
    }

    /// Прогоны проверок всех агентов по времени. Повтор — тот же набор на том же коде: в выводе тот же коммит
    /// у каждого набора, а если коммита в выводе нет — тот же агент не менял код с прошлого такого прогона.
    pub fn check_runs(&self) -> Vec<CheckRun> {
        let mut runs: Vec<CheckRun> = Vec::new();
        let owners = self.main.iter().chain(self.agents.iter().map(|a| &a.t));
        for (owner, t) in owners.enumerate() {
            for c in &t.calls {
                if !c.check {
                    continue;
                }
                let at = match t.result_at.get(&c.id) {
                    Some(&at) if truthy(at) => at,
                    _ => continue,
                };
                runs.push(CheckRun {
                    owner,
                    t: c.t,
                    dt: (at - c.t).max(0.0),
                    call: c.clone(),
                    repeat: false,
                    by_other: false,
                });
            }
        }
        runs.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(Ordering::Equal));

        let mut checked: HashMap<String, usize> = HashMap::new(); // набор@коммит → кто проверил первым
        let mut last_ver: HashMap<String, u32> = HashMap::new(); // агент и набор → версия его кода при прошлом прогоне
        for r in runs.iter_mut() {
            let owner = r.owner;
            let last_key = format!("{} {}", owner, r.call.suite);
            if !r.call.hashes.is_empty() {
                let by: Vec<Option<usize>> = r.call.hashes.iter().map(|k| checked.get(k).copied()).collect();
                r.repeat = by.iter().all(|x| x.is_some());
                r.by_other = r.repeat && by.iter().any(|x| *x != Some(owner));
                for k in &r.call.hashes {
                    checked.entry(k.clone()).or_insert(owner);
                }
            } else {
                r.repeat = last_ver.get(&last_key) == Some(&r.call.ver);
            }
            last_ver.insert(last_key, r.call.ver);
        }
        runs
    }

    /// Доля времени субагентов-проверяющих: тип агента или его задание про тесты, проверки, ревью.
    fn check_share(&self, now: f64) -> f64 {
        let mut all = 0.0;
        let mut mine = 0.0;
        for a in &self.agents {
            let ms = agent_time(a, now);
            all += ms;
            if is_check_role(&format!("{} {}", a.kind, a.name)) {
                mine += ms;
            }
        }
        if truthy(all) && truthy(mine) {
            mine / all
        } else {
            0.0
        }
    }
}
