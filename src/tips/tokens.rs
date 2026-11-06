//! Советы по экономии токенов: большие выводы, общие файлы, повторы команд, разросшиеся субагенты,
//! механика у главного агента, перезапись кэша и очень большой контекст.

use std::cmp::Ordering;
use std::collections::HashSet;

use indexmap::IndexMap;
use serde_json::json;

use super::{lead, Kind, Tip, TAIL};
use crate::fixes::{Change, Fix};
use crate::format::{plural, times, tok};
use crate::session::{short_model, Agent, Session};
use crate::text::clip;
use crate::transcript::{count_before, Call, Transcript, COMPACT_AT, PRICE};

/// Большой вывод, который перечитывается из контекста: сводка по команде или файлу.
struct Heavy {
    label: String,
    size: f64,
    rr: f64,
    cost: f64,
}

/// Файл, который читали субагенты.
struct SharedFile {
    label: String,
    n: i64,
    agents: HashSet<usize>,
    cost: f64,
}

/// Команда, которую один владелец запускал повторно. Владелец: -1 — главный агент, 0.. — субагенты.
struct Repeat {
    label: String,
    n: i64,
    agents: HashSet<i64>,
    cost: f64,
}

/// По убыванию: сортировка по цене, как b - a в JS.
fn desc(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

impl Session {
    /// Советы 1–7 по токенам, отсортированные по цене (w) по убыванию.
    pub(super) fn token_tips(&self) -> Vec<Tip> {
        let mut list = Vec::new();
        if let Some(main) = &self.main {
            let subs: Vec<&Transcript> = self.agents.iter().map(|a| &a.t).collect();
            let tail = TAIL;

            // 1. Большие выводы в контексте главного агента: каждый перечитывается из кэша на каждом запросе
            //    до следующего сжатия контекста.
            let req_t: Vec<f64> = main.reqs.iter().map(|r| r.t).collect();
            let rereads = |t: f64| -> f64 {
                let end = match main.compactions.iter().find(|&&x| x > t) {
                    Some(&c) => count_before(&req_t, c),
                    None => req_t.len(),
                };
                (end as f64 - count_before(&req_t, t + 1.0) as f64).max(0.0)
            };
            let mut heavy: IndexMap<String, Heavy> = IndexMap::new();
            for c in &main.calls {
                let size = match main.results.get(&c.id) {
                    Some(&s) if s >= 1500.0 => s,
                    _ => continue,
                };
                let rr = rereads(c.t);
                let key = if c.key.is_empty() { c.label.clone() } else { c.key.clone() };
                let h =
                    heavy.entry(key).or_insert_with(|| Heavy { label: c.label.clone(), size: 0.0, rr: 0.0, cost: 0.0 });
                h.size = h.size.max(size);
                h.rr = h.rr.max(rr);
                h.cost += size * rr;
            }
            let mut top: Vec<&Heavy> = heavy.values().filter(|h| h.cost >= 200e3).collect();
            top.sort_by(|a, b| desc(a.cost, b.cost));
            top.truncate(4);
            let heavy_cost: f64 = top.iter().map(|h| h.cost).sum();
            if heavy_cost >= 500e3 {
                let items: Vec<String> = top
                    .iter()
                    .map(|h| format!("{} (~{}, перечитан {})", clip(&h.label, 50), tok(h.size), times(h.rr as i64)))
                    .collect();
                let joined = items.join("; ");
                list.push(Tip {
                    key: "heavy".into(),
                    kind: Kind::Tokens,
                    title: "Большие выводы держатся в контексте".into(),
                    cost: heavy_cost,
                    unit: "из кэша",
                    send: true,
                    w: None,
                    detail: joined.clone(),
                    text: lead("heavy", "экономии токенов")
                        + &format!("в контексте этой сессии лежат большие выводы, они перечитываются из кэша на каждом запросе: {joined}. ")
                        + "Дальше читай файлы кусками (Read с offset/limit или grep -n по нужному разделу), длинные выводы команд обрезай "
                        + "(| tail -n 30, тихие флаги), а большие файлы разбирай через субагента и бери от него короткую выжимку."
                        + tail,
                    fix: None,
                });
            }

            // 2. Одни и те же файлы читают многие субагенты
            let mut files: IndexMap<String, SharedFile> = IndexMap::new();
            for (ai, t) in subs.iter().enumerate() {
                for c in &t.calls {
                    if !c.key.starts_with("read:") {
                        continue;
                    }
                    let f = files.entry(c.key.clone()).or_insert_with(|| SharedFile {
                        label: c.label.clone(),
                        n: 0,
                        agents: HashSet::new(),
                        cost: 0.0,
                    });
                    f.n += 1;
                    f.agents.insert(ai);
                    f.cost += t.results.get(&c.id).copied().unwrap_or(0.0);
                }
            }
            let mut shared: Vec<&SharedFile> = files.values().filter(|f| f.agents.len() >= 5).collect();
            shared.sort_by(|a, b| desc(a.cost, b.cost));
            shared.truncate(4);
            let shared_cost: f64 = shared.iter().map(|f| f.cost).sum();
            if shared_cost >= 200e3 {
                let items: Vec<String> = shared
                    .iter()
                    .map(|f| {
                        let n_agents = f.agents.len() as i64;
                        format!(
                            "{} ({} {}, {}, ~{})",
                            clip(&f.label, 40),
                            n_agents,
                            plural(n_agents, "агент", "агента", "агентов"),
                            times(f.n),
                            tok(f.cost)
                        )
                    })
                    .collect();
                let joined = items.join("; ");
                list.push(Tip {
                    key: "shared".into(),
                    kind: Kind::Tokens,
                    title: "Субагенты читают одни и те же файлы".into(),
                    cost: shared_cost,
                    unit: "на входе",
                    send: true,
                    w: None,
                    detail: joined.clone(),
                    text: lead("shared", "экономии токенов")
                        + &format!("субагенты этой сессии снова и снова читают одни и те же файлы: {joined}. ")
                        + "Когда запускаешь субагентов, передавай им в задании нужный фрагмент или путь к короткой выжимке "
                        + "(отдельный файл только с их разделом), чтобы каждый не читал общий файл целиком."
                        + tail,
                    fix: None,
                });
            }

            // 3. Повторяющиеся команды: каждый вызов — ещё один запрос, который заново читает весь контекст агента.
            //    Повтор — когда один агент сам запускает одно и то же 3+ раз; «каждый агент один раз закоммитил» — норма.
            // команда → (владелец → [контекст каждого вызова])
            let mut per_agent: IndexMap<String, IndexMap<i64, Vec<f64>>> = IndexMap::new();
            let mut add_cmds = |t: &Transcript, owner: i64| {
                for c in &t.calls {
                    if !c.key.starts_with("cmd:") {
                        continue;
                    }
                    per_agent.entry(c.key.clone()).or_default().entry(owner).or_default().push(c.ctx);
                }
            };
            add_cmds(main, -1);
            for (i, t) in subs.iter().enumerate() {
                add_cmds(t, i as i64);
            }
            let mut reps: Vec<Repeat> = Vec::new();
            for (key, by) in &per_agent {
                let mut r = Repeat { label: key[4..].to_string(), n: 0, agents: HashSet::new(), cost: 0.0 };
                for (owner, ctxs) in by {
                    if ctxs.len() < 3 {
                        continue;
                    }
                    r.n += ctxs.len() as i64;
                    r.agents.insert(*owner);
                    r.cost += ctxs.iter().sum::<f64>();
                }
                if r.n >= 8 {
                    reps.push(r);
                }
            }
            reps.sort_by(|a, b| desc(a.cost, b.cost));
            reps.truncate(3);
            let rep_cost: f64 = reps.iter().map(|r| r.cost).sum();
            if rep_cost >= 500e3 {
                let who = |r: &Repeat| -> String {
                    if r.agents.len() > 1 || !r.agents.contains(&-1) {
                        let n = r.agents.len() as i64;
                        format!(" у {} {}", n, plural(n, "агента", "агентов", "агентов"))
                    } else {
                        String::new()
                    }
                };
                let items: Vec<String> =
                    reps.iter().map(|r| format!("«{}» — {}{}", clip(&r.label, 60), times(r.n), who(r))).collect();
                let joined = items.join("; ");
                list.push(Tip {
                    key: "repeat".into(),
                    kind: Kind::Tokens,
                    title: "Одни и те же команды запускаются много раз".into(),
                    cost: rep_cost,
                    unit: "из кэша",
                    send: true,
                    w: None,
                    detail: joined.clone(),
                    text: lead("repeat", "экономии токенов")
                        + &format!("одни и те же команды запускаются много раз: {joined}. Каждый запуск — отдельный запрос модели, ")
                        + "который заново читает весь контекст. Сверни повторяющуюся последовательность в скрипт (например, в tools/), "
                        + "который делает всё за один вызов и печатает только итог, и давай субагентам готовую команду."
                        + tail,
                    fix: None,
                });
            }

            // 4. Разросшиеся субагенты: агент с контекстом за 200k дочитывает хвост работы в разы дороже свежего
            //    (замер из «Доклада по токенам»: 655k-агент — 135M за час, свежий доделал тот же хвост за 5,6M)
            let mut bloated: Vec<&Agent> = self.agents.iter().filter(|a| a.t.max_ctx >= 200e3).collect();
            let big_reads: f64 = bloated.iter().map(|a| a.t.big_reads).sum();
            if !bloated.is_empty() && big_reads >= 2e6 {
                bloated.sort_by(|a, b| desc(a.t.max_ctx, b.t.max_ctx));
                let items: Vec<String> = bloated
                    .iter()
                    .take(3)
                    .map(|a| format!("{} (до {})", clip(&a.name, 40), tok(a.t.max_ctx)))
                    .collect();
                let n = bloated.len() as i64;
                let joined = items.join("; ");
                list.push(Tip {
                    key: "bloated".into(),
                    kind: Kind::Tokens,
                    title: "Субагенты разрастаются".into(),
                    cost: big_reads,
                    unit: "из кэша при контексте >200k",
                    send: true,
                    w: Some(big_reads * PRICE.read),
                    detail: format!(
                        "{} {} с контекстом больше 200k: {}.",
                        n,
                        plural(n, "агент работал", "агента работали", "агентов работали"),
                        joined
                    ),
                    text: lead("bloated", "экономии токенов")
                        + &format!("субагенты этой сессии разрастаются: {joined}. Агент с большим контекстом перечитывает его на каждом шаге, ")
                        + "поэтому хвост работы у него стоит в разы дороже, чем у свежего. Давай субагентам задачи поменьше, не продолжай "
                        + "через SendMessage агента с контекстом за 150–200k, а запускай нового с короткой выжимкой того, что сделано."
                        + tail,
                    fix: None,
                });
            }

            // 5. Механика у главного агента: если он в основном гоняет команды, это дешевле делать субагенту на Sonnet
            let shell: Vec<&Call> = main.calls.iter().filter(|c| c.shell).collect();
            if shell.len() >= 100 && shell.len() as f64 >= 0.5 * main.calls.len() as f64 && main.model.contains("opus")
            {
                let cost: f64 = shell.iter().map(|c| c.ctx).sum();
                let short = short_model(&main.model);
                list.push(Tip {
                    key: "shell".into(),
                    kind: Kind::Tokens,
                    title: "Главный агент сам делает механику".into(),
                    cost,
                    unit: "из кэша на командах",
                    send: true,
                    w: Some(cost * PRICE.read),
                    detail: format!(
                        "{} из {} вызовов главного агента — команды (сборки, прогоны, слияния). Каждая перечитывает его контекст на {}.",
                        shell.len(),
                        main.calls.len(),
                        short
                    ),
                    text: lead("shell", "экономии токенов")
                        + &format!("{} из {} твоих вызовов — команды оболочки. Механическую работу (сборки, прогоны тестов, ", shell.len(), main.calls.len())
                        + "слияния, проверки) отдавай субагенту на Sonnet пачкой с понятным критерием готовности, а сам оставляй решения."
                        + tail,
                    fix: None,
                });
            }

            // 6. Перезапись кэша после пауз — агент тут не поможет, это для пользователя
            let all: Vec<&Transcript> = std::iter::once(main).chain(subs.iter().copied()).collect();
            let rewrites: i64 = all.iter().map(|t| t.rewrites as i64).sum();
            let rewrite_tok: f64 = all.iter().map(|t| t.rewrite_tok).sum();
            let subs_5m: f64 = subs.iter().map(|t| t.rewrite_5m).sum();
            if rewrite_tok >= 300e3 {
                // больше всего переписывают субагенты с 5-минутным кэшем
                let short = subs_5m > 0.3 * rewrite_tok;
                let mut tip = Tip {
                    key: "cache".into(),
                    kind: Kind::Tokens,
                    title: "Кэш переписывался заново".into(),
                    cost: rewrite_tok,
                    unit: "записи",
                    send: false,
                    w: Some(all.iter().map(|t| t.rewrite_w).sum()),
                    detail: format!(
                        "{} контекст записывался в кэш почти целиком: после паузы дольше срока жизни кэша или после \
                         сжатия контекста. Токенов немного, но запись в 12–20 раз дороже чтения.{} Перед долгой паузой дешевле сжать контекст (/compact) — это вручную.",
                        times(rewrites),
                        if short {
                            " Больше всего переписывают субагенты с 5-минутным кэшем — Enter: скриптом продлить их кэш до часа."
                        } else {
                            ""
                        }
                    ),
                    text: String::new(),
                    fix: None,
                };
                if short {
                    tip.fix = Some(Fix {
                        what: "кэш субагентов на час".into(),
                        changes: vec![Change {
                            path: vec!["subagentPromptCacheTtl".into()],
                            to: json!("1h"),
                            label: None,
                        }],
                        note: "для всех сессий: запись в кэш на час дороже (×20 к чтению вместо ×12,5), зато кэш субагента переживает паузы до часа; \
                               при перерасходе подписки час не действует"
                            .into(),
                    });
                }
                list.push(tip);
            }

            // 7. Очень большой контекст главного агента — тоже решение пользователя
            if self.ctx >= 300e3 {
                let m = main.model.as_str();
                let mut tip = Tip {
                    key: "ctx".into(),
                    kind: Kind::Tokens,
                    title: format!("Контекст главного агента {}", tok(self.ctx)),
                    cost: self.ctx,
                    unit: "на каждый запрос",
                    send: false,
                    w: Some(self.ctx * PRICE.read),
                    detail: "Каждый запрос перечитывает его целиком. Если начинаете новую большую задачу, помогут /compact или новая сессия — это вручную.".into(),
                    text: String::new(),
                    fix: None,
                };
                if m.starts_with("claude-") {
                    // окно автосжатия по модели — та же настройка, что сохраняет /autocompact
                    tip.detail += &format!(
                        " Enter — скриптом включить автосжатие раньше: на {} вместо почти 1M.",
                        tok(COMPACT_AT)
                    );
                    tip.fix = Some(Fix {
                        what: format!("автосжатие {} на {}", short_model(m), tok(COMPACT_AT)),
                        changes: vec![Change {
                            path: vec!["modelSettings".into(), m.into(), "autoCompactWindow".into()],
                            to: json!(COMPACT_AT as u64),
                            label: None,
                        }],
                        note: format!(
                            "для всех сессий на {}: разговор сжимается, дойдя до ~{} токенов; при сжатии старые подробности заменяются выжимкой",
                            short_model(m),
                            tok(COMPACT_AT)
                        ),
                    });
                }
                list.push(tip);
            }
        }

        // ранжируем по цене: чтение кэша ×0,1, вход в контекст ×1,25 (запись), записи — по их сроку жизни
        for t in list.iter_mut() {
            if t.w.is_none() {
                t.w = Some(t.cost * if t.unit == "на входе" { PRICE.write_5m } else { PRICE.read });
            }
        }
        list.sort_by(|a, b| desc(a.w.unwrap_or(0.0), b.w.unwrap_or(0.0)));
        list
    }
}
