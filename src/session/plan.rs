//! План сессии: шаги из инструментов, из строк «ccwatch: план» в ответе и из чек-листа в файле,
//! оценки времени в концах шагов и перенос отметок времени между версиями плана.

use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use super::{when_of, Ack, PlanSource, Session};
use crate::clock;
use crate::ideas::Idea;
use crate::json;
use crate::paths::Paths;

/// Статус шага. Пустая строка в данных означает «ожидает».
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Pending,
    InProgress,
    Completed,
    Other(String),
}

impl Status {
    pub fn from_str(s: &str) -> Status {
        match s {
            "" | "pending" => Status::Pending,
            "in_progress" => Status::InProgress,
            "completed" => Status::Completed,
            other => Status::Other(other.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Step {
    /// номер объекта: по нему видно, тот же это шаг или новый (в JS сравнивали ссылки)
    pub uid: u64,
    pub content: String,
    pub status: Status,
    /// номер задачи TaskCreate: нужен, чтобы TaskUpdate нашёл шаг
    pub id: Option<String>,
    /// оценка в миллисекундах, 0 — оценки нет
    pub est: f64,
    pub started_at: f64,
    pub done_at: f64,
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// Оценка в конце шага: «~15м», «(~2h)», «~1ч 30м»; текст шага без неё.
pub fn step(content: &str, status: &str) -> Step {
    let mut text = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut est = 0.0;
    let cut = crate::re!(
        r"\s*[(\[]?\s*[~≈]\s*(\d+(?:[.,]\d+)?)\s*(ч|час\S*|h|hr|м|мин\S*|m|min)\.?(?:\s*(\d+)\s*(м|мин\S*|m|min)\.?)?\s*[)\]]?\s*$",
        "i"
    )
    .captures(&text)
    .map(|m| {
        let n: f64 = m[1].replace(',', ".").parse().unwrap_or(0.0);
        let unit = m[2].to_lowercase();
        let hours = unit.starts_with('ч') || unit.starts_with('h');
        let mut e = (if hours { n * 60.0 } else { n }) * 60e3;
        if let Some(mins) = m.get(3) {
            e += mins.as_str().parse::<f64>().unwrap_or(0.0) * 60e3;
        }
        (m.get(0).map_or(0, |g| g.start()), e)
    });
    if let Some((start, e)) = cut {
        est = e;
        text = text[..start]
            .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '—' | '–' | '-'))
            .trim()
            .to_string();
    }
    Step {
        uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
        content: text,
        status: Status::from_str(status),
        id: None,
        est,
        started_at: 0.0,
        done_at: 0.0,
    }
}

/// Строка плана из текста ответа: «1. ✓ Шаг ~15м», жирный markdown убираем.
pub fn text_step(s: &str) -> Step {
    let mut t = s.replace("**", "").trim().to_string();
    let mut status = "pending";
    if let Some(rest) = t.strip_prefix(|c: char| c == '✓' || c == '✔') {
        status = "completed";
        t = rest.trim_start().to_string();
    } else if let Some(rest) = t.strip_prefix('▶') {
        status = "in_progress";
        t = rest.trim_start().to_string();
    }
    step(&t, status)
}

/// Идея из строки «ccwatch: идея …» (без самого слова): до трёх частей через тире, недостающие пустые.
/// Без названия идеи нет — None.
fn parse_idea(rest: &str, t: f64) -> Option<Idea> {
    let parts: Vec<&str> = crate::re!(r"\s[—–-]\s").splitn(rest.trim(), 3).collect();
    let part = |k: usize| parts.get(k).map_or(String::new(), |p| p.trim().to_string());
    let name = part(0);
    // пустое название (строка начинается с тире) — это не идея
    if name.is_empty() || name.starts_with(['—', '–', '-']) {
        return None;
    }
    Some(Idea { t, name, detect: part(1), advice: part(2) })
}

/// Номер шага из JSON (считается с 1) → индекс. Дробные, нулевые и выходящие за план номера пропускаем,
/// как JS, где plan[n - 1] для них undefined.
fn slot(n: &Value, len: usize) -> Option<usize> {
    let f = n.as_f64()?;
    if f.fract() != 0.0 || f < 1.0 {
        return None;
    }
    let k = f as usize - 1;
    (k < len).then_some(k)
}

/// Применяет update_plan десктоп-приложения: полный список items или сдвиги advance / done / start.
pub fn apply_plan(plan: Vec<Step>, i: &Value) -> Vec<Step> {
    let mut plan = plan;
    if let Some(items) = i.get("items").and_then(Value::as_array) {
        plan = items.iter().map(|x| step(&json::text(x, "content"), &json::text(x, "status"))).collect();
    }
    if let Some(done) = i.get("done").and_then(Value::as_array) {
        for n in done {
            if let Some(k) = slot(n, plan.len()) {
                plan[k].status = Status::Completed;
            }
        }
    }
    if json::has(i, "advance") {
        let cur: isize = plan.iter().position(|x| x.status == Status::InProgress).map_or(-1, |p| p as isize);
        if cur >= 0 {
            plan[cur as usize].status = Status::Completed;
        }
        // следующий после текущего ожидающий; если такого нет — первый ожидающий
        let next = plan
            .iter()
            .enumerate()
            .position(|(k, x)| k as isize > cur && x.status == Status::Pending)
            .or_else(|| plan.iter().position(|x| x.status == Status::Pending));
        if let Some(k) = next {
            plan[k].status = Status::InProgress;
        }
    }
    if let Some(k) = i.get("start").and_then(|v| slot(v, plan.len())) {
        plan[k].status = Status::InProgress;
    }
    plan
}

/// Оценка шага, который вышел за свой срок.
#[derive(Clone, Debug)]
pub struct Over {
    pub content: String,
    pub est: f64,
    pub spent: f64,
}

#[derive(Clone, Debug)]
pub struct Estimate {
    pub total: f64,
    pub left: f64,
    pub over: Option<Over>,
    pub spent: f64,
    pub missing: usize,
    pub all_done: bool,
}

impl Session {
    /// Строка «ccwatch: …» в тексте ответа: план, ответ на совет или метки шагов.
    pub(crate) fn on_text(&mut self, text: &str, at: &str) {
        let when = when_of(at);
        let prev = self.plan.clone();
        let lines: Vec<String> = text.split('\n').map(|l| l.replace('`', "").trim().to_string()).collect();
        let mut changed = false;
        let mut k = 0;
        while k < lines.len() {
            let line = lines[k].as_str();
            if crate::re!(r"^ccwatch:\s*план\s*:?$", "i").is_match(line) {
                let mut steps = Vec::new();
                while k + 1 < lines.len() {
                    if let Some(n) = crate::re!(r"^(\d+)[.)]\s+(.+)$").captures(&lines[k + 1]) {
                        steps.push(text_step(&n[2]));
                    } else if !lines[k + 1].is_empty() || !steps.is_empty() {
                        break;
                    }
                    k += 1;
                }
                if !steps.is_empty() {
                    self.plan = steps;
                    changed = true;
                }
            } else if let Some(a) =
                crate::re!(r"^ccwatch:\s*(принято|не применимо|отклонено)\s+(\w+)\s*(?:[—–:-]\s*(.*))?$", "i")
                    .captures(line)
            {
                // ответ на совет: «ccwatch: принято repeat — сведу прогоны в скрипт»
                let text = a.get(3).map_or("", |m| m.as_str()).trim().to_string();
                let ok = a[1].to_lowercase().contains("принято");
                self.acks.insert(a[2].to_string(), Ack { t: when, ok, text });
            } else if let Some(idea_line) = crate::re!(r"^ccwatch:\s*идея\s+(.+)$", "i").captures(line) {
                // идея для анализатора: «ccwatch: идея <название> — как заметить — что советовать»
                if let Some(idea) = parse_idea(&idea_line[1], when) {
                    self.ideas_in_text.push(idea);
                    self.merge_ideas();
                }
            } else if crate::re!(r"^ccwatch:\s*идей\s+нет", "i").is_match(line) {
                self.ideas_none_at = when;
            } else if let Some(mk) = crate::re!(r"^ccwatch:\s*((?:[✓✔▶]\s*\d+[\s,;]*)+)$").captures(line) {
                // метки шагов: «ccwatch: ✓2 ▶3»
                if !self.plan.is_empty() {
                    for c in crate::re!(r"([✓✔▶])\s*(\d+)").captures_iter(&mk[1]) {
                        let Some(idx) = c[2].parse::<usize>().ok().and_then(|v| v.checked_sub(1)) else { continue };
                        if idx >= self.plan.len() {
                            continue;
                        }
                        if &c[1] == "▶" {
                            for (j, y) in self.plan.iter_mut().enumerate() {
                                if j != idx && y.status == Status::InProgress {
                                    y.status = Status::Completed;
                                }
                            }
                            self.plan[idx].status = Status::InProgress;
                        } else {
                            self.plan[idx].status = Status::Completed;
                        }
                    }
                    changed = true;
                }
            }
            k += 1;
        }
        if changed {
            self.plan_changed(prev, when, PlanSource::Text);
        }
    }

    /// Чек-лист «- [x] / - [>] / - [ ]» из ~/.ccwatch/plans/<id>.md (так просила первая версия запроса).
    /// Берём его, только если в транскрипте плана нет: сессия, получившая оба запроса, ведёт и то и другое.
    pub(crate) fn read_plan_file(&mut self, paths: &Paths) {
        if matches!(self.plan_source, PlanSource::Tool | PlanSource::Text) {
            return;
        }
        let f = paths.plans.join(format!("{}.md", self.id));
        let Ok(meta) = fs::metadata(&f) else { return };
        let mtime = clock::mtime_ms(&meta);
        if mtime == self.plan_file_at {
            return;
        }
        self.plan_file_at = mtime;
        // файл может быть не в UTF-8: читаем с заменой битых байтов, как Node
        let text = fs::read(&f).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let mut steps = Vec::new();
        for line in text.split('\n') {
            if let Some(m) = crate::re!(r"^\s*[-*+]\s*\[([ xX>~])\]\s*(.+?)\s*$").captures(line) {
                let mark = &m[1];
                let status = if mark == " " {
                    "pending"
                } else if mark.contains(['x', 'X']) {
                    "completed"
                } else {
                    "in_progress"
                };
                steps.push(step(&m[2], status));
            }
        }
        if steps.is_empty() {
            return;
        }
        let prev = self.plan.clone();
        self.plan = steps;
        self.plan_changed(prev, mtime, PlanSource::File);
    }

    /// Файл ответа агента ~/.ccwatch/ideas/<id>.md: идеи пишутся инструментом, поэтому их не переписывает
    /// оформитель, как текст ответа. Читаем только при новом mtime; идеи из файла заменяют прежние из файла,
    /// так что перезапись не дублирует. «идей нет» без идей ставит ideas_none_at.
    pub(crate) fn read_ideas_file(&mut self, paths: &Paths) {
        let f = paths.ideas_dir.join(format!("{}.md", self.id));
        let Ok(meta) = fs::metadata(&f) else { return };
        let mtime = clock::mtime_ms(&meta);
        if mtime == self.ideas_file_at {
            return;
        }
        self.ideas_file_at = mtime;
        let text = fs::read(&f).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let mut ideas = Vec::new();
        let mut none = false;
        for raw in text.lines() {
            let line = raw.replace('`', "");
            // маркеры списка: «- », «* », «1. », «1) »
            let body = match crate::re!(r"^(?:[-*+]|\d+[.)])\s+(.*)$").captures(line.trim()) {
                Some(m) => m[1].trim().to_string(),
                None => line.trim().to_string(),
            };
            // необязательный префикс «ccwatch: идея »
            let body = match crate::re!(r"^ccwatch:\s*идея\s+(.*)$", "i").captures(&body) {
                Some(m) => m[1].trim().to_string(),
                None => body.clone(),
            };
            if body.is_empty() {
                continue;
            }
            if crate::re!(r"^(?:ccwatch:\s*)?идей\s+нет\.?$", "i").is_match(&body) {
                none = true;
                continue;
            }
            if let Some(idea) = parse_idea(&body, mtime) {
                ideas.push(idea);
            }
        }
        if ideas.is_empty() && none {
            self.ideas_none_at = mtime;
        }
        self.ideas_in_file = ideas;
        self.merge_ideas();
    }

    /// Общий список идей сессии: из строк ответа и из файла, по времени.
    pub(crate) fn merge_ideas(&mut self) {
        let mut all: Vec<Idea> = self.ideas_in_text.iter().chain(self.ideas_in_file.iter()).cloned().collect();
        all.sort_by(|a, b| a.t.total_cmp(&b.t));
        self.ideas = all;
    }

    /// Переносит отметки времени со старой версии плана (шаги совпадают по тексту) и ставит
    /// новые по смене статусов: так видно, сколько на самом деле занял каждый шаг.
    pub(crate) fn plan_changed(&mut self, prev: Vec<Step>, at: f64, source: PlanSource) {
        // при одинаковом тексте побеждает последний шаг, как в Map из JS
        let old: HashMap<&str, &Step> = prev.iter().map(|x| (x.content.as_str(), x)).collect();
        let mut matched = 0;
        for x in self.plan.iter_mut() {
            let Some(&o) = old.get(x.content.as_str()) else { continue };
            matched += 1;
            if o.uid == x.uid {
                continue;
            }
            if x.started_at == 0.0 {
                x.started_at = o.started_at;
            }
            if x.done_at == 0.0 {
                x.done_at = o.done_at;
            }
            if x.est == 0.0 {
                x.est = o.est;
            }
        }
        if matched == 0 || self.plan_born_at == 0.0 {
            self.plan_born_at = at;
        }
        let mut last_done = self.plan_born_at;
        for x in self.plan.iter_mut() {
            if x.status != Status::Completed {
                x.done_at = 0.0;
            }
            if x.status == Status::InProgress && x.started_at == 0.0 {
                x.started_at = at;
            }
            if x.status == Status::Completed {
                if x.done_at == 0.0 {
                    x.done_at = at;
                }
                if x.started_at == 0.0 {
                    x.started_at = last_done.min(x.done_at);
                }
                last_done = last_done.max(x.done_at);
            }
        }
        self.plan_at = at;
        if source != PlanSource::None {
            self.plan_source = source;
        }
    }

    /// Оценки шагов: всего, сколько осталось, какой шаг вышел за оценку.
    pub fn estimate(&self, now: f64) -> Option<Estimate> {
        let with_est: Vec<&Step> = self.plan.iter().filter(|x| x.est != 0.0).collect();
        if with_est.is_empty() {
            return None;
        }
        let mut total = 0.0;
        let mut left = 0.0;
        let mut over = None;
        for x in &with_est {
            total += x.est;
            if x.status == Status::Completed {
                continue;
            }
            let spent = if x.status == Status::InProgress && x.started_at != 0.0 { now - x.started_at } else { 0.0 };
            left += (x.est - spent).max(0.0);
            if spent > x.est {
                over = Some(Over { content: x.content.clone(), est: x.est, spent });
            }
        }
        let all_done = self.plan.iter().all(|x| x.status == Status::Completed);
        // у законченного плана время считается до последнего завершения, у живого — до сейчас
        let end = if all_done { self.plan.iter().map(|x| x.done_at).fold(f64::NEG_INFINITY, f64::max) } else { now };
        Some(Estimate {
            total,
            left,
            over,
            spent: (end - self.plan_born_at).max(0.0),
            missing: self.plan.len() - with_est.len(),
            all_done,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Время строки транскрипта для разбора ответа.
    const AT: &str = "2026-10-18T10:00:00.000Z";

    #[test]
    fn idea_line_has_three_parts() {
        let mut s = Session::new("sid");
        let text = "Вот мои идеи.\nccwatch: идея тесты без кэша — tsc в каждом прогоне, минуты — держать кэш компиляции\nконец";
        s.on_text(text, AT);
        assert_eq!(s.ideas.len(), 1);
        let i = &s.ideas[0];
        assert_eq!(i.name, "тесты без кэша");
        assert_eq!(i.detect, "tsc в каждом прогоне, минуты");
        assert_eq!(i.advice, "держать кэш компиляции");
        assert!(i.t > 0.0);
    }

    #[test]
    fn idea_line_with_two_parts_and_en_dash() {
        let mut s = Session::new("sid");
        s.on_text("ccwatch: идея повторные сборки – одна команда дважды подряд", AT);
        assert_eq!(s.ideas.len(), 1);
        assert_eq!(s.ideas[0].name, "повторные сборки");
        assert_eq!(s.ideas[0].detect, "одна команда дважды подряд");
        assert_eq!(s.ideas[0].advice, "");
    }

    #[test]
    fn idea_line_one_part_and_hyphen_separator() {
        let mut s = Session::new("sid");
        s.on_text("CCWATCH: Идея поллинг логов\nccwatch: идея опрос - sleep в цикле - заменить на ожидание", AT);
        assert_eq!(s.ideas.len(), 2);
        assert_eq!(s.ideas[0].name, "поллинг логов");
        assert_eq!(s.ideas[0].detect, "");
        assert_eq!(s.ideas[1].advice, "заменить на ожидание");
        // без названия строка не идея
        s.on_text("ccwatch: идея — пусто", AT);
        assert_eq!(s.ideas.len(), 2);
    }

    #[test]
    fn no_ideas_line_sets_time() {
        let mut s = Session::new("sid");
        s.on_text("ccwatch: идей нет", AT);
        assert!(s.ideas.is_empty());
        assert_eq!(s.ideas_none_at, clock::parse_iso(AT).unwrap());
    }

    #[test]
    fn ideas_file_parses_markers_and_replaces_on_rewrite() {
        let dir = std::env::temp_dir().join(format!("ccwatch-unit-{}-ideas-file", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = Paths::new(&dir.join("claude"), &dir.join("ccwatch"));
        fs::create_dir_all(&paths.ideas_dir).unwrap();
        let f = paths.ideas_dir.join("sid.md");
        // между записями ждём, чтобы mtime точно изменился
        let pause = || std::thread::sleep(std::time::Duration::from_millis(30));

        fs::write(&f, "- тесты без кэша — tsc в каждом прогоне — держать кэш\n2) ccwatch: идея опрос — sleep в цикле\n* повторы — одна команда\n").unwrap();
        let mut s = Session::new("sid");
        s.read_ideas_file(&paths);
        assert_eq!(s.ideas.len(), 3);
        assert_eq!(s.ideas[0].name, "тесты без кэша");
        assert_eq!(s.ideas[0].advice, "держать кэш");
        assert_eq!(s.ideas[1].name, "опрос");
        assert_eq!(s.ideas[2].detect, "одна команда");
        assert!(s.ideas[0].t > 0.0, "время идеи из файла — его mtime");

        // перезапись заменяет прежние идеи из файла, а не дописывает к ним
        pause();
        fs::write(&f, "1. один — два — три\n").unwrap();
        s.read_ideas_file(&paths);
        assert_eq!(s.ideas.len(), 1);
        assert_eq!(s.ideas[0].name, "один");

        // та же запись без изменения mtime — не перечитываем
        s.read_ideas_file(&paths);
        assert_eq!(s.ideas.len(), 1);

        // «идей нет» — отметка времени, идей нет
        pause();
        fs::write(&f, "- идей нет\n").unwrap();
        s.read_ideas_file(&paths);
        assert!(s.ideas.is_empty());
        assert!(s.ideas_none_at > 0.0);
        assert_eq!(s.ideas_file_at, s.ideas_none_at);

        // идеи из строк ответа и из файла вместе, по времени
        s.on_text("ccwatch: идея из текста — что-то — и это", AT);
        assert_eq!(s.ideas.len(), 1);
        assert_eq!(s.ideas[0].name, "из текста");

        let _ = fs::remove_dir_all(&dir);
    }
}
