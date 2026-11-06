//! Эффект доставленных советов: у каждого совета своя мера до доставки (за 3 часа) и после неё.
//! Это корреляция, а не доказательство: мера может сдвинуться и от смены фазы задачи.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use super::is_service_key;
use super::speed::{hit_limit, truthy, CheckRun};
use crate::format::{dur, pct, plural, tok};
use crate::session::{Agent, Session};
use crate::style::{Rgb, AMBER, DIM, FAINT, GREEN};
use crate::transcript::commands::is_trivial;
use crate::transcript::{Call, Transcript};

/// Как показывать значение меры.
#[derive(Clone, Copy, Debug)]
pub enum Fmt {
    /// Токены, как tok().
    Tokens,
    /// Доля, как pct().
    Percent,
    /// Длительность, как dur().
    Duration,
    /// Секунды с одним знаком: «12.3 с».
    Seconds,
}

impl Fmt {
    pub fn apply(self, x: f64) -> String {
        match self {
            Fmt::Tokens => tok(x),
            Fmt::Percent => pct(x),
            Fmt::Duration => dur(x),
            Fmt::Seconds => format!("{} с", crate::format::to_fixed(x / 1000.0, 1)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Verdict {
    pub text: String,
    pub color: Rgb,
}

#[derive(Clone, Debug)]
pub struct Effect {
    pub key: String,
    /// Время последней доставки совета.
    pub t: f64,
    /// Сколько прошло от доставки до сейчас.
    pub since: f64,
    pub what: &'static str,
    pub before: f64,
    pub after: f64,
    pub fmt: Fmt,
    pub n: String,
    pub enough: bool,
    pub verdict: Verdict,
    /// Общая мера по всем агентам: (до, после); None — данных мало.
    pub overall: Option<(f64, f64)>,
}

/// Мера совета до и после доставки (до её расчёта итог ещё не известен).
struct Measure {
    what: &'static str,
    before: f64,
    after: f64,
    fmt: Fmt,
    n: String,
    enough: bool,
}

/// Предикат по времени: попадает ли момент в окно «до» или «после».
type When<'a> = &'a dyn Fn(f64) -> bool;

/// Итог «до / после» для меры, где меньше — лучше.
pub fn verdict(before: f64, after: f64, enough: bool) -> Verdict {
    if !enough || !truthy(before) {
        return Verdict { text: "мало данных".into(), color: FAINT };
    }
    let change = (after - before) / before;
    if change <= -0.2 {
        return Verdict {
            text: format!("помогло ({}%)", crate::format::num(crate::format::js_round(change * 100.0))),
            color: GREEN,
        };
    }
    if change >= 0.2 {
        return Verdict {
            text: format!("стало хуже (+{}%)", crate::format::num(crate::format::js_round(change * 100.0))),
            color: AMBER,
        };
    }
    Verdict { text: "без заметной разницы".into(), color: DIM }
}

/// Субагенты, у которых первая реплика попала в окно и которые проходят условие keep.
fn started<'a>(agents: &'a [Agent], inw: When, keep: impl Fn(&Agent) -> bool) -> Vec<&'a Agent> {
    agents.iter().filter(|a| keep(*a) && inw(a.t.first)).collect()
}

/// Среднее значение по субагентам из списка.
fn mean(list: &[&Agent], per: impl Fn(&Agent) -> f64) -> f64 {
    if list.is_empty() {
        0.0
    } else {
        list.iter().map(|&a| per(a)).sum::<f64>() / list.len() as f64
    }
}

fn agents_measure(agents: &[Agent], bloated: bool, shared: &HashSet<String>, b: When, a: When) -> Measure {
    // разрастание — макс. контекст агента; общие файлы — сколько на них ушло токенов у агента
    let per = |ag: &Agent| -> f64 {
        if bloated {
            ag.t.max_ctx
        } else {
            ag.t.calls
                .iter()
                .filter(|c| shared.contains(&c.key))
                .map(|c| ag.t.results.get(&c.id).copied().unwrap_or(0.0))
                .sum()
        }
    };
    let before = started(agents, b, |ag| truthy(ag.t.first));
    let after = started(agents, a, |ag| truthy(ag.t.first));
    let n = after.len() as i64;
    Measure {
        what: if bloated {
            "макс. контекст нового субагента"
        } else {
            "чтение общих файлов на субагента"
        },
        before: mean(&before, &per),
        after: mean(&after, &per),
        fmt: Fmt::Tokens,
        n: format!("{} {}", n, plural(n, "новый агент", "новых агента", "новых агентов")),
        enough: after.len() >= 2 && before.len() >= 1,
    }
}

/// Доля повторных запусков: каждый запуск той же программы тем же агентом сверх первого.
/// Возвращает (доля, сколько запусков).
fn repeat_share(owners: &[&Transcript], inw: When) -> (f64, i64) {
    let mut total = 0_i64;
    let mut rep = 0_i64;
    for o in owners {
        let mut by: HashMap<&str, i64> = HashMap::new();
        for c in &o.calls {
            if c.key.starts_with("cmd:") && inw(c.t) {
                *by.entry(c.key.as_str()).or_insert(0) += 1;
            }
        }
        for n in by.values() {
            total += n;
            rep += n - 1;
        }
    }
    (if total > 0 { rep as f64 / total as f64 } else { 0.0 }, total)
}

/// Вызовы главного агента в окне.
fn main_calls<'a>(main: &'a Transcript, inw: When) -> Vec<&'a Call> {
    main.calls.iter().filter(|c| inw(c.t)).collect()
}

fn main_measure(main: &Transcript, heavy: bool, b: When, a: When) -> Measure {
    // тяжёлый вывод — средний размер результата; механика — доля команд оболочки
    let value = |list: &[&Call]| -> f64 {
        if list.is_empty() {
            0.0
        } else if heavy {
            list.iter().map(|c| main.results.get(&c.id).copied().unwrap_or(0.0)).sum::<f64>() / list.len() as f64
        } else {
            list.iter().filter(|c| c.shell).count() as f64 / list.len() as f64
        }
    };
    let before = main_calls(main, b);
    let after = main_calls(main, a);
    let n = after.len() as i64;
    Measure {
        what: if heavy {
            "средний вывод инструмента у главного агента"
        } else {
            "доля команд у главного агента"
        },
        before: value(&before[..]),
        after: value(&after[..]),
        fmt: if heavy { Fmt::Tokens } else { Fmt::Percent },
        n: format!("{} {}", n, plural(n, "вызов", "вызова", "вызовов")),
        enough: after.len() >= 20 && before.len() >= 10,
    }
}

/// Скорость на один вызов инструмента: доля мелких команд или секунды долгих проверок / ожидания в цикле.
/// Возвращает (значение, сколько вызовов).
fn per_call(owners: &[&Transcript], key: &str, inw: When) -> (f64, i64) {
    let mut n = 0_i64;
    let mut v = 0.0;
    for o in owners {
        for c in &o.calls {
            if !inw(c.t) {
                continue;
            }
            n += 1;
            if !c.shell {
                continue;
            }
            let dt = match o.result_at.get(&c.id) {
                Some(&at) if truthy(at) => (at - c.t).max(0.0),
                _ => 0.0,
            };
            match key {
                "smallshell" => {
                    if !c.poll && is_trivial(&c.prog) {
                        v += 1.0;
                    }
                }
                "polling" => {
                    if c.poll {
                        v += dt;
                    }
                }
                _ => {
                    if !c.poll && !is_trivial(&c.prog) && dt >= 60e3 {
                        v += dt;
                    }
                }
            }
        }
    }
    (if n > 0 { v / n as f64 } else { 0.0 }, n)
}

/// Доля повторов среди прогонов проверок. Возвращает (доля, сколько прогонов).
fn recheck_share(runs: &[CheckRun], inw: When) -> (f64, i64) {
    let list: Vec<&CheckRun> = runs.iter().filter(|r| inw(r.t)).collect();
    let n = list.len() as i64;
    let v = if list.is_empty() { 0.0 } else { list.iter().filter(|r| r.repeat).count() as f64 / list.len() as f64 };
    (v, n)
}

/// Время команд, упёршихся в лимит, на одну команду. Возвращает (среднее время, сколько команд).
fn timeout_per(owners: &[&Transcript], inw: When) -> (f64, i64) {
    let mut n = 0_i64;
    let mut v = 0.0;
    for o in owners {
        for c in &o.calls {
            if !c.shell || !inw(c.t) {
                continue;
            }
            let at = match o.result_at.get(&c.id) {
                Some(&at) if truthy(at) => at,
                _ => continue,
            };
            n += 1;
            if hit_limit(c, at - c.t) {
                v += at - c.t;
            }
        }
    }
    (if n > 0 { v / n as f64 } else { 0.0 }, n)
}

/// Среднее отношение размышлений ко времени модели у субагентов из списка.
fn think_share(list: &[&Agent]) -> f64 {
    let ms: f64 = list.iter().map(|a| a.t.model_ms()).sum();
    if truthy(ms) {
        list.iter().map(|a| a.t.think_ms).sum::<f64>() / ms
    } else {
        0.0
    }
}

/// Средний контекст запроса по всем владельцам в окне. Возвращает (среднее, сколько запросов).
fn ctx_avg(owners: &[&Transcript], inw: When) -> (f64, i64) {
    let mut s = 0.0;
    let mut n = 0_i64;
    for o in owners {
        for r in &o.reqs {
            if inw(r.t) {
                s += r.ctx;
                n += 1;
            }
        }
    }
    (if n > 0 { s / n as f64 } else { 0.0 }, n)
}

impl Session {
    /// Эффекты доставленных советов, по убыванию времени доставки. Пересчёт не чаще раза в 5 секунд.
    pub fn effects(&self, now: f64) -> Vec<Effect> {
        let cached =
            self.effects_cache.borrow().as_ref().filter(|(at, _)| now - at < 5000.0).map(|(_, list)| list.clone());
        if let Some(list) = cached {
            return list;
        }
        let out = self.compute_effects(now);
        *self.effects_cache.borrow_mut() = Some((now, out.clone()));
        out
    }

    fn compute_effects(&self, now: f64) -> Vec<Effect> {
        let mut out = Vec::new();
        let main = match &self.main {
            Some(m) if !m.deliveries.is_empty() => m,
            _ => return out,
        };
        // по каждому совету берём последнюю доставку
        let mut latest: IndexMap<String, f64> = IndexMap::new();
        for d in &main.deliveries {
            if is_service_key(&d.key) {
                continue;
            }
            let newer = latest.get(&d.key).map_or(true, |&old| d.t > old);
            if newer {
                latest.insert(d.key.clone(), d.t);
            }
        }

        let agents: &[Agent] = &self.agents;
        // главный агент и субагенты, по порядку
        let mut owners: Vec<&Transcript> = vec![main];
        owners.extend(agents.iter().map(|a| &a.t));

        // файлы, которые за сессию читали 5+ субагентов
        let mut readers: HashMap<String, HashSet<usize>> = HashMap::new();
        for (i, a) in agents.iter().enumerate() {
            for c in &a.t.calls {
                if c.key.starts_with("read:") {
                    readers.entry(c.key.clone()).or_default().insert(i);
                }
            }
        }
        let shared: HashSet<String> = readers.iter().filter(|(_, s)| s.len() >= 5).map(|(k, _)| k.clone()).collect();

        // прогоны проверок нужны только совету «повторные проверки»
        let runs = if latest.contains_key("recheck") { self.check_runs() } else { Vec::new() };

        for (key, &t) in latest.iter() {
            let from = t - 3.0 * 3600e3;
            let in_b = |x: f64| x >= from && x < t;
            let in_a = |x: f64| x >= t;
            let (b, a): (When, When) = (&in_b, &in_a);
            let measure = match key.as_str() {
                "bloated" | "shared" => Some(agents_measure(agents, key == "bloated", &shared, b, a)),
                "repeat" => {
                    let (bv, bn) = repeat_share(&owners, b);
                    let (av, an) = repeat_share(&owners, a);
                    Some(Measure {
                        what: "доля повторных запусков",
                        before: bv,
                        after: av,
                        fmt: Fmt::Percent,
                        n: format!("{} {}", an, plural(an, "запуск", "запуска", "запусков")),
                        enough: an >= 20 && bn >= 10,
                    })
                }
                "heavy" | "shell" => Some(main_measure(main, key == "heavy", b, a)),
                "slowcheck" | "smallshell" | "polling" => {
                    let (bv, bn) = per_call(&owners, key, b);
                    let (av, an) = per_call(&owners, key, a);
                    Some(Measure {
                        what: match key.as_str() {
                            "smallshell" => "доля мелких команд оболочки",
                            "polling" => "ожидание в цикле на вызов",
                            _ => "долгие проверки на вызов",
                        },
                        before: bv,
                        after: av,
                        fmt: if key == "smallshell" { Fmt::Percent } else { Fmt::Seconds },
                        n: format!("{} {}", an, plural(an, "вызов", "вызова", "вызовов")),
                        enough: an >= 30 && bn >= 30,
                    })
                }
                "recheck" => {
                    let (bv, bn) = recheck_share(&runs, b);
                    let (av, an) = recheck_share(&runs, a);
                    Some(Measure {
                        what: "доля повторных прогонов проверок",
                        before: bv,
                        after: av,
                        fmt: Fmt::Percent,
                        n: format!("{} {}", an, plural(an, "прогон", "прогона", "прогонов")),
                        enough: an >= 10 && bn >= 10,
                    })
                }
                "timeout" => {
                    let (bv, bn) = timeout_per(&owners, b);
                    let (av, an) = timeout_per(&owners, a);
                    Some(Measure {
                        what: "упор в тайм-аут на команду",
                        before: bv,
                        after: av,
                        fmt: Fmt::Seconds,
                        n: format!("{} {}", an, plural(an, "команда", "команды", "команд")),
                        enough: an >= 30 && bn >= 30,
                    })
                }
                "ramp" => {
                    let before = started(agents, b, |ag| ag.t.ramp_ms >= 0.0);
                    let after = started(agents, a, |ag| ag.t.ramp_ms >= 0.0);
                    let n = after.len() as i64;
                    Some(Measure {
                        what: "разгон нового субагента до первой правки",
                        before: mean(&before, |ag| ag.t.ramp_ms),
                        after: mean(&after, |ag| ag.t.ramp_ms),
                        fmt: Fmt::Duration,
                        n: format!("{} {}", n, plural(n, "новый агент", "новых агента", "новых агентов")),
                        enough: after.len() >= 2 && before.len() >= 1,
                    })
                }
                "thinking" => {
                    let before = started(agents, b, |_| true);
                    let after = started(agents, a, |_| true);
                    let n = after.len() as i64;
                    Some(Measure {
                        what: "доля размышлений у новых субагентов",
                        before: think_share(&before),
                        after: think_share(&after),
                        fmt: Fmt::Percent,
                        n: format!("{} {}", n, plural(n, "новый агент", "новых агента", "новых агентов")),
                        enough: after.len() >= 2 && before.len() >= 2,
                    })
                }
                _ => None,
            };
            let Some(mut m) = measure else { continue };

            // общая мера: средний контекст запроса по всем агентам
            let (cb, cn_b) = ctx_avg(&owners, b);
            let (ca, cn_a) = ctx_avg(&owners, a);
            let since = now - t;
            m.enough = m.enough && since >= 15.0 * 60e3;
            out.push(Effect {
                key: key.clone(),
                t,
                since,
                what: m.what,
                before: m.before,
                after: m.after,
                fmt: m.fmt,
                n: m.n,
                enough: m.enough,
                verdict: verdict(m.before, m.after, m.enough),
                overall: if cn_a >= 30 && cn_b >= 30 { Some((cb, ca)) } else { None },
            });
        }
        out.sort_by(|a, b| b.t.partial_cmp(&a.t).unwrap_or(std::cmp::Ordering::Equal));
        out
    }
}
