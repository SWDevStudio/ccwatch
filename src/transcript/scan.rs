//! Поиск строк и меток прямо в байтах. Строки транскрипта бывают многомегабайтными,
//! и декодировать весь кусок в текст в разы дольше, чем найти нужное.
//! Позиции — байтовые, как в Node Buffer: indexOf / lastIndexOf возвращают -1, если не нашли.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexSet;
use memchr::{memchr, memmem, memrchr};

use crate::clock;
use crate::format::{js_round, num};
use crate::re;

use super::{By, Call, Delivery, SkillUse, Transcript, BYTES_PER_TOKEN, IMG_TOKENS};

const MARK: &[u8] = b"\"type\":\"a"; // начало "type":"assistant" и "type":"ai-title"
const TASK_MARK: &[u8] = b"\"toolUseResult\":{\"task\":{"; // ответ TaskCreate с номером задачи
const RESULT_MARK: &[u8] = b"\"type\":\"tool_result\""; // вывод инструмента; перед ним стоит "tool_use_id"
const TUR_MARK: &[u8] = b"\"toolUseResult\""; // копия вывода для интерфейса — её не считаем
const IMG_MARK: &[u8] = b"\"type\":\"image\"";
const TIP_MARK: &[u8] = b"(ccwatch:"; // метка совета в тексте, который хук доставил сессии
const TS_MARK: &[u8] = b"\"timestamp\":\"";
const CMD_MARK: &[u8] = b"<command-name>"; // команда, набранная через «/»
const USER_MARK: &[u8] = b"\"type\":\"user\"";
const ATTACH_MARK: &[u8] = b"\"type\":\"attachment\""; // доставка хука лежит в строке-вложении с временем
const QUEUE_MARK: &[u8] = b"\"type\":\"queue-operation\""; // сообщение или уведомление из очереди
const QUEUED_MARK: &[u8] = b"\"attachment\":{\"type\":\"queued_command\""; // его время — когда набрали, а не когда отдали модели

/// Позиция от from (включительно) или -1.
pub(super) fn find(buf: &[u8], needle: &[u8], from: i64) -> i64 {
    let from = from.max(0) as usize;
    if from > buf.len() {
        return -1;
    }
    memmem::find(&buf[from..], needle).map_or(-1, |i| (i + from) as i64)
}

/// Последнее вхождение, НАЧАЛО которого не позже at, или -1 (как lastIndexOf(needle, at)).
pub(super) fn rfind(buf: &[u8], needle: &[u8], at: i64) -> i64 {
    if at < 0 {
        return -1;
    }
    let end = (at as usize).saturating_add(needle.len()).min(buf.len());
    memmem::rfind(&buf[..end], needle).map_or(-1, |i| i as i64)
}

/// Перевод строки не позже at (как lastIndexOf(10, at)).
pub(super) fn last_nl(buf: &[u8], at: i64) -> i64 {
    if at < 0 {
        return -1;
    }
    let end = ((at as usize) + 1).min(buf.len());
    memrchr(b'\n', &buf[..end]).map_or(-1, |i| i as i64)
}

/// Перевод строки от from (как indexOf(10, from)).
pub(super) fn find_nl(buf: &[u8], from: i64) -> i64 {
    let from = from.max(0) as usize;
    if from >= buf.len() {
        return -1;
    }
    memchr(b'\n', &buf[from..]).map_or(-1, |i| (i + from) as i64)
}

/// Срез buf[a..b] с обрезкой границ, как buf.subarray / toString: пустой, если b ≤ a.
pub(super) fn sl(buf: &[u8], a: i64, b: i64) -> &[u8] {
    let n = buf.len() as i64;
    let a = a.clamp(0, n);
    let b = b.clamp(0, n);
    if b <= a {
        &buf[0..0]
    } else {
        &buf[a as usize..b as usize]
    }
}

/// buf.toString('utf8', …): битые байты — символ замены.
pub(super) fn utf8(s: &[u8]) -> String {
    String::from_utf8_lossy(s).into_owned()
}

/// buf.toString('latin1', …): каждый байт — один символ.
pub(super) fn latin1(s: &[u8]) -> String {
    s.iter().map(|&b| b as char).collect()
}

/// Дата из 24 байт сразу за меткой "timestamp":" (Date.parse || 0).
fn stamp(buf: &[u8], ts: i64) -> f64 {
    let a = ts + TS_MARK.len() as i64;
    clock::parse_iso(&latin1(sl(buf, a, a + 24))).unwrap_or(0.0)
}

/// Короткий хеш коммита: первые 7 символов в нижнем регистре.
fn short_hash(h: &str) -> String {
    h.chars().take(7).collect::<String>().to_lowercase()
}

/// Строки, которые нужны монитору: начала assistant / ai-title, а с withTasks — ответы TaskCreate.
/// Возвращает [начало, конец) нужных строк в порядке их следования.
pub(super) fn find_lines(buf: &[u8], end: i64, with_tasks: bool) -> Vec<(i64, i64)> {
    let mut found = BTreeMap::new();
    grab(buf, end, MARK, &|k| k == "ssistant\"" || k.starts_with("i-title\""), &mut found);
    if with_tasks {
        grab(buf, end, TASK_MARK, &|_| true, &mut found);
    }
    found.into_iter().collect()
}

/// Находит строки с меткой mark, которые проходят проверку ok (по 9 байтам после метки).
fn grab(buf: &[u8], end: i64, mark: &[u8], ok: &dyn Fn(&str) -> bool, found: &mut BTreeMap<i64, i64>) {
    let mut pos = 0i64;
    loop {
        pos = find(buf, mark, pos);
        if pos < 0 || pos >= end {
            break;
        }
        let after = pos + mark.len() as i64;
        if !ok(&latin1(sl(buf, after, after + 9))) {
            pos = after;
            continue;
        }
        let from = last_nl(buf, pos) + 1;
        let mut to = find_nl(buf, pos);
        if to < 0 || to > end {
            to = end;
        }
        found.insert(from, to);
        pos = to + 1;
    }
}

impl Transcript {
    /// Команды, набранные человеком через «/» (среди них и скиллы): строка пользователя с <command-name>.
    pub(super) fn scan_commands(&mut self, buf: &[u8], end: i64) {
        let mut pos = 0i64;
        loop {
            pos = find(buf, CMD_MARK, pos);
            if pos < 0 || pos >= end {
                break;
            }
            let from = last_nl(buf, pos) + 1;
            let mut to = find_nl(buf, pos);
            if to < 0 || to > end {
                to = end;
            }
            let head = utf8(sl(buf, pos, to.min(pos + 300)));
            let user = find(buf, USER_MARK, from);
            let ts = rfind(buf, TS_MARK, to);
            pos = to + 1;
            if user < 0 || user > to || ts < from {
                continue;
            }
            let Some(m) = re!(r"<command-name>/?([^<]+)</command-name>").captures(&head) else { continue };
            let body = utf8(sl(buf, from, to.min(from + 4000)));
            let args = re!(r"<command-args>([^<]*)</command-args>")
                .captures(&body)
                .map_or(String::new(), |a| a[1].trim().to_string());
            let name = m[1].trim().to_string();
            let t = stamp(buf, ts);
            if t != 0.0 && self.fresh(format!("cmd:{}@{}", name, num(t))) {
                self.skills.push(SkillUse { name, args, t, by: By::User });
            }
        }
    }

    /// Доставленные хуком советы: строка-вложение с меткой «(ccwatch:<совет>)». Та же метка встречается
    /// и в служебных строках запроса, но без времени — их пропускаем.
    pub(super) fn scan_deliveries(&mut self, buf: &[u8], end: i64) {
        let mut pos = 0i64;
        loop {
            pos = find(buf, TIP_MARK, pos);
            if pos < 0 || pos >= end {
                break;
            }
            let from = last_nl(buf, pos) + 1;
            let mut to = find_nl(buf, pos);
            if to < 0 || to > end {
                to = end;
            }
            pos = to + 1;
            let a = find(buf, ATTACH_MARK, from);
            if a < 0 || a > to || to - from > 500_000 {
                continue;
            }
            let line = utf8(sl(buf, from, to));
            let t =
                re!(r#""timestamp":"([^"]+)""#).captures(&line).map_or(0.0, |c| clock::parse_iso(&c[1]).unwrap_or(0.0));
            if t == 0.0 {
                continue;
            }
            let keys: IndexSet<String> =
                re!(r"\(ccwatch:(\w+)\)").captures_iter(&line).map(|c| c[1].to_string()).collect();
            for key in keys {
                if self.fresh(format!("tip:{}@{}", key, num(t))) {
                    self.deliveries.push(Delivery { key, t });
                }
            }
        }
    }

    /// Размеры выводов инструментов считаем по байтам, не разбирая JSON: строки с выводами — основная
    /// масса транскрипта (у субагентов это сотни мегабайт), а точность до токена тут не нужна.
    pub(super) fn scan_results(&mut self, buf: &[u8], end: i64) {
        let mut pos = 0i64;
        loop {
            pos = find(buf, RESULT_MARK, pos);
            if pos < 0 || pos >= end {
                break;
            }
            let mut line_end = find_nl(buf, pos);
            if line_end < 0 || line_end > end {
                line_end = end;
            }
            let mut next = find(buf, RESULT_MARK, pos + RESULT_MARK.len() as i64);
            if next < 0 || next > line_end {
                next = line_end;
            }
            let mut seg_end = find(buf, TUR_MARK, pos);
            if seg_end < 0 || seg_end > next {
                seg_end = next;
            }
            let id =
                re!(r#""tool_use_id":"([^"]+)""#).captures(&latin1(sl(buf, pos - 100, pos))).map(|c| c[1].to_string());
            let images = count_images(buf, pos, seg_end);
            if let Some(id) = &id {
                let tokens = if images > 0 {
                    images as f64 * IMG_TOKENS
                } else {
                    js_round((seg_end - pos) as f64 / BYTES_PER_TOKEN)
                };
                self.results.insert(id.clone(), tokens);
            }
            // время результата — для длительности вызова; верхнее поле строки, оно идёт в её конце
            let ts_pos = rfind(buf, TS_MARK, line_end);
            if let Some(id) = &id {
                if ts_pos > last_nl(buf, pos) {
                    let t = stamp(buf, ts_pos);
                    if t != 0.0 {
                        self.result_at.insert(id.clone(), t);
                    }
                }
            }
            if let Some(idx) = id.as_ref().and_then(|id| self.by_id.get(id).copied()) {
                let (shell, check) = (self.calls[idx].shell, self.calls[idx].check);
                if shell {
                    let limit = if check { 6000 } else { 600 };
                    let head = utf8(sl(buf, pos, seg_end.min(pos + limit)));
                    Self::read_output(&mut self.calls[idx], &head);
                }
            }
            pos = if next < line_end { next } else { line_end + 1 };
        }
    }

    /// Начало вывода команды: упёрлась ли она в тайм-аут и на каком коммите шла проверка.
    /// Проверки, которые печатают «набор: pass <хеш>», сравниваем по каждому набору; иначе — по «HEAD <хеш>».
    fn read_output(c: &mut Call, head: &str) {
        let text = re!(r"\\[nrt]").replace_all(head, " ").replace("\\\"", "\"");
        let head500: String = text.chars().take(500).collect();
        if re!(r"\bCommand timed out after\b").is_match(&head500) {
            c.timed_out = true; // «timed out» в выводе тестов — не то
        }
        if !c.check {
            return;
        }
        let mut keys: IndexSet<String> = IndexSet::new();
        for m in
            re!(r"([\w.\/-]+):\s+(?:pass|passed|ok|fail|failed|error|skip|skipped|cached)\w*\s+([0-9a-f]{7,40})\b", "i")
                .captures_iter(&text)
        {
            keys.insert(format!("{}@{}", &m[1], short_hash(&m[2])));
        }
        let head_hash = if keys.is_empty() {
            re!(r"\b(?:HEAD|commit|sha|коммит|хеш)\b[\s:=@]+([0-9a-f]{7,40})\b", "i").captures(&text)
        } else {
            None
        };
        if let Some(h) = head_hash {
            keys.insert(format!("{}@{}", c.suite, short_hash(&h[1])));
        }
        if !keys.is_empty() {
            c.hashes = keys.into_iter().collect();
        }
    }

    /// Вводы модели: сообщения, уведомления, взятые из очереди, и вложения (вывод хуков, напоминания) —
    /// с них начинается запрос. Результаты инструментов собирает scan_results. Постановка в очередь
    /// (enqueue и вложение queued_command) — время набора, модель его ещё не получила.
    pub(super) fn scan_inputs(&mut self, buf: &[u8], end: i64) {
        let mut done: HashSet<i64> = HashSet::new(); // строка с несколькими метками — одна
        for mark in [ATTACH_MARK, QUEUE_MARK, USER_MARK] {
            let mut pos = 0i64;
            loop {
                pos = find(buf, mark, pos);
                if pos < 0 || pos >= end {
                    break;
                }
                // результат инструмента виден сразу за меткой — такие строки (бывают в мегабайты) не размечаем
                if mark == USER_MARK && find(sl(buf, pos, pos + 400), RESULT_MARK, 0) >= 0 {
                    pos += mark.len() as i64;
                    continue;
                }
                let from = last_nl(buf, pos) + 1;
                let mut to = find_nl(buf, pos);
                if to < 0 || to > end {
                    to = end;
                }
                let at = pos;
                pos = to + 1;
                if !done.insert(from) {
                    continue;
                }
                if mark == ATTACH_MARK && find(sl(buf, from, at.min(from + 300)), QUEUED_MARK, 0) >= 0 {
                    continue;
                }
                if mark == QUEUE_MARK
                    && latin1(sl(buf, at + mark.len() as i64, at + mark.len() as i64 + 24)).contains("enqueue")
                {
                    continue;
                }
                let ts = rfind(buf, TS_MARK, to);
                if ts < from {
                    continue;
                }
                let t = stamp(buf, ts);
                if t == 0.0 {
                    continue;
                }
                self.inputs.push(t);
                if self.start_at == 0.0 || t < self.start_at {
                    self.start_at = t;
                }
            }
        }
    }
}

/// Сколько картинок в выводе инструмента: метки "type":"image" с началом раньше конца сегмента.
/// Поиск ограничен окном [pos, segEnd + длина метки): начало метки, попавшее в окно, — тоже учитываем.
fn count_images(buf: &[u8], pos: i64, seg_end: i64) -> u32 {
    let mut images = 0;
    let mut ip = pos;
    loop {
        let f = find(sl(buf, ip, seg_end + IMG_MARK.len() as i64), IMG_MARK, 0);
        if f < 0 {
            break;
        }
        ip += f;
        if ip >= seg_end {
            break;
        }
        images += 1;
        ip += IMG_MARK.len() as i64;
    }
    images
}
