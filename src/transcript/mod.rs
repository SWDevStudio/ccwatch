//! Транскрипт сессии: строки журнала читаются кусками с места прошлой остановки
//! и сводятся в токены, запросы к модели, вызовы инструментов и активное время.
//! Большие файлы не перечитываются: смещение хранится между обновлениями.

pub mod commands;
pub mod entry;
mod scan;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use indexmap::IndexMap;

use crate::clock;
use crate::json;

use commands::{
    call_key, call_label, changes_code, cmd_core, is_check, is_edit_tool, is_note_file, is_poll, is_trivial, prog_name,
    suite_name,
};
use entry::Usage;
pub use entry::{Action, Block, Entry};

/// Перерыв дольше — агент не работал: ждал или получил позднее уведомление.
pub const PAUSE: f64 = 1_800_000.0;
/// «Ответ» дольше — это пауза, а не работа модели.
pub const MODEL_CAP: f64 = 600_000.0;
/// Нормальный разгон субагента: прочитать задание и нужные файлы.
pub const RAMP_OK: f64 = 180_000.0;
/// Тайм-аут команды по умолчанию; больше 10 минут поставить нельзя.
pub const SHELL_LIMIT: f64 = 120_000.0;
/// Картинка в контексте — около 1,6k токенов, сколько бы ни весил base64.
pub const IMG_TOKENS: f64 = 1600.0;
/// Грубо: смесь кода, английского и русского.
pub const BYTES_PER_TOKEN: f64 = 3.3;

/// Цена относительно обычного ввода (одинаково у всех моделей Claude).
pub struct Price {
    pub read: f64,
    pub write_5m: f64,
    pub write_1h: f64,
    pub input: f64,
    pub output: f64,
}

/// Советы ранжируем по этой цене: чтение кэша — почти все токены, но запись в кэш
/// при ~4% токенов даёт ~40% денег.
pub const PRICE: Price = Price { read: 0.1, write_5m: 1.25, write_1h: 2.0, input: 1.0, output: 5.0 };

/// Окно автосжатия, которое предлагает совет про большой контекст (настройка — от 100k до 1M).
pub const COMPACT_AT: f64 = 400_000.0;

/// Токены: вход, вход по цене, из кэша, выход.
#[derive(Clone, Copy, Default, Debug)]
pub struct Tokens {
    pub i: f64,
    pub wi: f64,
    pub c: f64,
    pub o: f64,
}

impl Tokens {
    pub fn add(&mut self, b: &Tokens) {
        self.i += b.i;
        self.wi += b.wi;
        self.c += b.c;
        self.o += b.o;
    }

    /// Стоимость в единицах обычного ввода.
    pub fn cost(&self) -> f64 {
        self.wi + self.c * PRICE.read + self.o * PRICE.output
    }
}

/// Вызов инструмента для советов: что за команда, сколько шла, на каком контексте.
#[derive(Clone, Debug, Default)]
pub struct Call {
    pub id: String,
    pub key: String,
    pub label: String,
    pub shell: bool,
    pub prog: String,
    pub poll: bool,
    pub t: f64,
    pub ctx: f64,
    pub check: bool,
    pub suite: String,
    pub ver: u32,
    pub limit: f64,
    pub bg: bool,
    pub timed_out: bool,
    pub hashes: Vec<String>,
}

/// Запрос к модели: когда, на каком контексте, когда пришёл последний кусок ответа.
#[derive(Clone, Debug)]
pub struct Req {
    pub t: f64,
    pub ctx: f64,
    pub end: f64,
    pub tools: u32,
}

/// Доставленный хуком совет.
#[derive(Clone, Debug)]
pub struct Delivery {
    pub key: String,
    pub t: f64,
}

/// Кто вызвал скилл: сам агент (инструмент Skill) или человек через «/».
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum By {
    Agent,
    User,
}

#[derive(Clone, Debug)]
pub struct SkillUse {
    pub name: String,
    pub args: String,
    pub t: f64,
    pub by: By,
}

/// Состояние чтения одного транскрипта. Поля, которые нужны другим модулям, — pub;
/// остальное — служебное для дочитывания и подсчёта.
pub struct Transcript {
    pub file: PathBuf,
    pub offset: u64,
    pub mtime: f64,
    /// время первой и последней реплики агента
    pub first: f64,
    pub last_at: f64,
    pub model: String,
    /// последний инструмент: ставит take() для каждого tool_use (нужно субагентам)
    pub last: Option<Action>,
    /// модель → токены
    pub by_model: IndexMap<String, Tokens>,
    // для советов по экономии
    pub calls: Vec<Call>,
    /// tool_use id → примерный размер вывода в токенах
    pub results: HashMap<String, f64>,
    pub reqs: Vec<Req>,
    /// когда контекст резко сжимался (/compact)
    pub compactions: Vec<f64>,
    /// запросы, где кэш пришлось записать почти целиком
    pub rewrites: u32,
    pub rewrite_tok: f64,
    /// из них записано с 5-минутным сроком жизни (у субагентов это по умолчанию)
    pub rewrite_5m: f64,
    /// те же записи по цене ввода: 5 минут ×1,25, час ×2
    pub rewrite_w: f64,
    pub max_ctx: f64,
    /// чтение кэша в запросах, где контекст больше 200k
    pub big_reads: f64,
    pub deliveries: Vec<Delivery>,
    // для советов по скорости
    /// tool_use id → когда пришёл результат
    pub result_at: HashMap<String, f64>,
    /// сколько модель размышляла
    pub think_ms: f64,
    /// «модель effort» → мс размышлений: какую настройку менять
    pub think_by: IndexMap<String, f64>,
    pub effort: String,
    pub skills: Vec<SkillUse>,
    /// сколько повторов пропущено
    pub skipped: u32,
    /// самый ранний ввод (у субагента — его задание)
    pub start_at: f64,
    /// когда модель получала ввод: сообщение, уведомление, вложение (результаты — в result_at)
    pub inputs: Vec<f64>,
    /// сколько раз агент менял код: правки, запись в файлы, git-операции
    pub ver: u32,
    /// активное время до первой правки кода; -1 — правок не было
    pub ramp_ms: f64,

    // служебное
    /// message.id → (токены, модель): одно сообщение пишется несколькими строками
    usage: HashMap<String, (Tokens, String)>,
    /// message.id → индекс запроса в reqs
    req_by_id: HashMap<String, usize>,
    /// uuid уже разобранных строк
    seen: HashSet<String>,
    /// самое позднее время среди них
    max_at: f64,
    /// уже учтённые команды через «/» и доставки советов: «имя@время»
    marks: HashSet<String>,
    /// начало текущего куска активного времени
    chunk_at: f64,
    /// сумма закрытых кусков
    active_ms: f64,
    /// tool_use id → индекс вызова в calls
    by_id: HashMap<String, usize>,
    /// кэш model_ms: ключ (число запросов, вводов, результатов, последнее время) → мс
    model_cache: Cell<Option<((usize, usize, usize, u64), f64)>>,
}

impl Transcript {
    pub fn new(file: PathBuf) -> Transcript {
        Transcript::blank(file, 0.0)
    }

    /// Пустое состояние; mtime сохраняется отдельно — его обновляет pull() до сброса.
    fn blank(file: PathBuf, mtime: f64) -> Transcript {
        Transcript {
            file,
            offset: 0,
            mtime,
            first: 0.0,
            last_at: 0.0,
            model: String::new(),
            last: None,
            by_model: IndexMap::new(),
            calls: Vec::new(),
            results: HashMap::new(),
            reqs: Vec::new(),
            compactions: Vec::new(),
            rewrites: 0,
            rewrite_tok: 0.0,
            rewrite_5m: 0.0,
            rewrite_w: 0.0,
            max_ctx: 0.0,
            big_reads: 0.0,
            deliveries: Vec::new(),
            result_at: HashMap::new(),
            think_ms: 0.0,
            think_by: IndexMap::new(),
            effort: String::new(),
            skills: Vec::new(),
            skipped: 0,
            start_at: 0.0,
            inputs: Vec::new(),
            ver: 0,
            ramp_ms: -1.0,
            usage: HashMap::new(),
            req_by_id: HashMap::new(),
            seen: HashSet::new(),
            max_at: 0.0,
            marks: HashSet::new(),
            chunk_at: 0.0,
            active_ms: 0.0,
            by_id: HashMap::new(),
            model_cache: Cell::new(None),
        }
    }

    /// Файл переписали — читаем заново.
    fn reset(&mut self) {
        let mtime = self.mtime;
        let file = std::mem::take(&mut self.file);
        *self = Transcript::blank(file, mtime);
    }

    /// Новая метка, если такой ещё не было: повтор (копия строки после resume) — false.
    fn fresh(&mut self, mark: String) -> bool {
        self.marks.insert(mark)
    }

    /// Дочитывает файл с сохранённого смещения и отдаёт каждую новую строку on_line.
    /// with_tasks — главный транскрипт: там же ищутся доставки советов и команды через «/».
    pub fn pull(&mut self, with_tasks: bool, on_line: &mut dyn FnMut(&Entry)) {
        let Ok(meta) = std::fs::metadata(&self.file) else { return };
        self.mtime = clock::mtime_ms(&meta);
        let size = meta.len();
        if size < self.offset {
            self.reset();
        }
        if size == self.offset {
            return;
        }
        let Ok(mut file) = File::open(&self.file) else { return };
        // кусок растёт, если в нём нет перевода строки (строка длиннее куска)
        let mut chunk: u64 = 4 << 20;
        while self.offset < size {
            let len = chunk.min(size - self.offset);
            if file.seek(SeekFrom::Start(self.offset)).is_err() {
                break;
            }
            let mut buf = vec![0u8; len as usize];
            let n = match file.read(&mut buf) {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                break;
            }
            let data = &buf[..n];
            // до последнего перевода строки: хвост строки ещё дописывается
            let end = memchr::memrchr(b'\n', data).map_or(-1, |i| i as i64);
            if end < 0 {
                if self.offset + n as u64 >= size {
                    break;
                }
                chunk *= 2;
                continue;
            }
            // первыми — вводы: начало работы агента нужно уже первой реплике
            self.scan_inputs(data, end);
            for (from, to) in scan::find_lines(data, end, with_tasks) {
                let text = String::from_utf8_lossy(scan::sl(data, from, to));
                let Ok(entry) = serde_json::from_str::<Entry>(&text) else { continue };
                if self.repeated(&entry) {
                    continue;
                }
                self.take(&entry);
                on_line(&entry);
            }
            self.scan_results(data, end);
            if with_tasks {
                // главный транскрипт: туда хук и доставляет советы
                self.scan_deliveries(data, end);
                // «/» набирает только человек — это тоже в главной сессии
                self.scan_commands(data, end);
            }
            self.offset += end as u64 + 1;
        }
    }

    /// Копия строки после resume: uuid уже встречался или время раньше уже прочитанного (с запасом в минуту).
    /// Без этого вызовы, скиллы и шаги плана из старой части журнала считались бы второй раз.
    fn repeated(&mut self, d: &Entry) -> bool {
        if !d.uuid.is_empty() {
            if !self.seen.insert(d.uuid.clone()) {
                self.skipped += 1;
                return true;
            }
        }
        let t = d.time();
        if t != 0.0 && t < self.max_at - 60e3 {
            self.skipped += 1;
            return true;
        }
        if t > self.max_at {
            self.max_at = t;
        }
        false
    }

    /// Активное время идёт кусками: перерыв дольше PAUSE не считаем — иначе позднее уведомление
    /// субагенту через несколько часов добавляло бы эти часы к его работе.
    fn mark(&mut self, at: f64) {
        if self.chunk_at == 0.0 {
            self.chunk_at = if self.start_at != 0.0 && self.start_at <= at && at - self.start_at <= PAUSE {
                self.start_at
            } else {
                at
            };
        } else if at - self.last_at > PAUSE {
            self.active_ms += self.last_at - self.chunk_at;
            self.chunk_at = at;
        }
    }

    /// Активное время к моменту at.
    pub fn active_at(&self, at: f64) -> f64 {
        if self.chunk_at != 0.0 {
            self.active_ms + (at - self.chunk_at).max(0.0)
        } else {
            0.0
        }
    }

    /// Время модели: от ввода (результат инструмента, сообщение, уведомление, вложение) до конца ответа.
    /// Ввод раньше конца прошлого ответа — не этого запроса: тогда считаем только сам ответ;
    /// дольше MODEL_CAP — это была пауза, а не работа модели, и тоже считаем только ответ.
    pub fn model_ms(&self) -> f64 {
        let key = (self.reqs.len(), self.inputs.len(), self.result_at.len(), self.last_at.to_bits());
        if let Some((k, ms)) = self.model_cache.get() {
            if k == key {
                return ms;
            }
        }
        let mut inp: Vec<f64> = self.inputs.clone();
        inp.extend(self.result_at.values().copied());
        inp.sort_by(f64::total_cmp);
        let mut prev_end = 0.0;
        let mut ms = 0.0;
        for r in &self.reqs {
            let i = count_before(&inp, r.t + 1.0) as i64 - 1;
            let from = if i >= 0 && inp[i as usize] > prev_end { inp[i as usize] } else { r.t };
            let d = (r.end - from).max(0.0);
            ms += if d <= MODEL_CAP { d } else { MODEL_CAP.min((r.end - r.t).max(0.0)) };
            prev_end = f64::max(prev_end, r.end);
        }
        self.model_cache.set(Some((key, ms)));
        ms
    }

    /// Разбор одной строки журнала: токены, запросы, вызовы инструментов, скиллы, размышления.
    fn take(&mut self, d: &Entry) {
        let Some(m) = d.message.as_ref() else { return };
        if d.kind != "assistant" {
            return;
        }
        let at = d.time();
        if at != 0.0 {
            self.mark(at);
        }
        if at != 0.0 && (self.first == 0.0 || at < self.first) {
            self.first = at;
        }
        if at > self.last_at {
            self.last_at = at;
        }
        if !m.model.is_empty() && !m.model.starts_with('<') {
            self.model = m.model.clone();
        }
        let u = m.usage.as_ref();
        let ctx = u.map_or(0.0, |u| u.context());
        if !d.effort.is_empty() {
            self.effort = d.effort.clone();
        }
        if d.thinking_ms != 0.0 {
            self.think_ms += d.thinking_ms;
            let by = format!("{} {}", or_q(&self.model), or_q(&self.effort));
            *self.think_by.entry(by).or_insert(0.0) += d.thinking_ms;
        }

        let mut tools = 0u32;
        for b in d.tool_uses() {
            tools += 1;
            // субагенту нужен последний инструмент для строки «Сейчас»
            self.last = Some(Action::of(b, &d.timestamp));
            self.note_skill(b, at);
            self.add_call(b, at, ctx);
        }

        let Some(u) = u else { return };
        if m.id.is_empty() {
            return;
        }
        let (w5m, w1h) = u.writes();
        let next = Tokens {
            i: u.input_tokens + u.cache_creation_input_tokens,
            wi: u.input_tokens * PRICE.input + w5m * PRICE.write_5m + w1h * PRICE.write_1h,
            c: u.cache_read_input_tokens,
            o: u.output_tokens,
        };
        let model = or_q(&self.model).to_string();
        // одно сообщение пишется несколькими строками: прошлую версию вычитаем, новую прибавляем
        match self.usage.insert(m.id.clone(), (next, model.clone())) {
            Some((old, old_model)) => self.bump(&old, &old_model, -1.0),
            None => {
                if ctx != 0.0 {
                    let idx = self.request(at, ctx, u);
                    self.req_by_id.insert(m.id.clone(), idx);
                }
            }
        }
        self.bump(&next, &model, 1.0);
        if let Some(&idx) = self.req_by_id.get(&m.id) {
            let r = &mut self.reqs[idx];
            r.end = r.end.max(at);
            r.tools += tools;
        }
    }

    /// Инструмент Skill: какой скилл вызвал агент.
    fn note_skill(&mut self, b: &Block, at: f64) {
        if b.name != "Skill" {
            return;
        }
        let i = &b.input;
        let name = json::first_truthy(i, &["skill", "name"]);
        self.skills.push(SkillUse {
            name: if name.is_empty() { "?".to_string() } else { name },
            args: json::first_truthy(i, &["args"]),
            t: at,
            by: By::Agent,
        });
    }

    /// Вызов инструмента: для Bash/PowerShell — проверка или нет, правка кода или нет.
    fn add_call(&mut self, b: &Block, at: f64, ctx: f64) {
        let i = &b.input;
        let shell = b.name == "Bash" || b.name == "PowerShell";
        let cmd = if shell { json::first_truthy(i, &["command"]) } else { String::new() };
        let core = if shell { cmd_core(&cmd) } else { String::new() };
        let prog = prog_name(&core);
        let poll = shell && is_poll(&cmd);
        // core — без тела heredoc
        let check = shell && !poll && !is_trivial(&prog) && is_check(&core);
        // правка кода (не заметок) или команда, меняющая файлы, — дальше проверки идут уже на другом коде
        let file = json::first_truthy(i, &["file_path", "notebook_path"]);
        let edit = is_edit_tool(&b.name) && !is_note_file(&file);
        if edit || (shell && !check && changes_code(&cmd)) {
            self.ver += 1;
        }
        if edit && self.ramp_ms < 0.0 {
            self.ramp_ms = self.active_at(at);
        }
        let idx = self.calls.len();
        self.calls.push(Call {
            id: b.id.clone(),
            key: call_key(b),
            label: call_label(b),
            shell,
            prog,
            poll,
            t: at,
            ctx,
            check,
            suite: if check { suite_name(&core) } else { String::new() },
            ver: self.ver,
            limit: if shell { json::number(i, "timeout") } else { 0.0 },
            bg: i.get("run_in_background").is_some_and(json::truthy),
            timed_out: false,
            hashes: Vec::new(),
        });
        self.by_id.insert(b.id.clone(), idx);
    }

    /// Новый запрос к модели: следим за сжатием контекста, полной перезаписью кэша и раздутым контекстом.
    fn request(&mut self, at: f64, ctx: f64, u: &Usage) -> usize {
        let last_ctx = self.reqs.last().map(|r| r.ctx);
        let written = u.cache_creation_input_tokens;
        if let Some(lc) = last_ctx {
            if lc > 50e3 && ctx < 0.6 * lc {
                self.compactions.push(at);
            }
        }
        // первый запрос всегда пишет кэш с нуля, это не потеря
        if last_ctx.is_some() && ctx > 50e3 && written > 0.5 * ctx {
            let (m5, h1) = u.writes();
            self.rewrites += 1;
            self.rewrite_tok += written;
            self.rewrite_5m += m5;
            self.rewrite_w += m5 * PRICE.write_5m + h1 * PRICE.write_1h;
        }
        if ctx > self.max_ctx {
            self.max_ctx = ctx;
        }
        if ctx > 200e3 {
            self.big_reads += u.cache_read_input_tokens;
        }
        self.reqs.push(Req { t: at, ctx, end: at, tools: 0 });
        self.reqs.len() - 1
    }

    /// Прибавляет (k = 1) или вычитает (k = -1) токены модели.
    fn bump(&mut self, t: &Tokens, model: &str, k: f64) {
        let b = self.by_model.entry(model.to_string()).or_default();
        b.i += k * t.i;
        b.wi += k * t.wi;
        b.c += k * t.c;
        b.o += k * t.o;
    }

    /// Сумма токенов по всем моделям.
    pub fn totals(&self) -> Tokens {
        let mut t = Tokens::default();
        for b in self.by_model.values() {
            t.add(b);
        }
        t
    }
}

/// Пустая строка → «?» (как `x || '?'` в JS).
fn or_q(s: &str) -> &str {
    if s.is_empty() {
        "?"
    } else {
        s
    }
}

/// Сколько элементов отсортированного массива меньше t (бинарный поиск).
/// Тот же цикл, что в JS: массив может быть не отсортирован, и результат должен совпасть.
pub fn count_before(arr: &[f64], t: f64) -> usize {
    let mut lo = 0;
    let mut hi = arr.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if arr[mid] < t {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
