#!/usr/bin/env node
/*
 * ccwatch — живой монитор сессий Claude Code (CLI и десктоп-приложение).
 *
 * Показывает запущенные сессии, шаги плана со шкалой прогресса, текущее действие,
 * субагентов с их моделями и временем работы и потраченные токены по моделям.
 * Без зависимостей. Читает:
 *   ~/.claude/sessions/<pid>.json           реестр живых сессий и их статус
 *   ~/.claude/projects/<проект>/<id>.jsonl  транскрипт: usage, вызовы плана, инструменты
 *   ~/.claude/projects/<проект>/<id>/subagents/*.jsonl  транскрипты субагентов
 * План берётся из update_plan / TodoWrite / TaskCreate или из строк «ccwatch: план» и
 * «ccwatch: ✓2 ▶3» в ответах агента; оценки времени — из «~15м» в конце шагов.
 * Пока монитор открыт, он просит план у работающих сессий без плана: кладёт запрос в
 * ~/.ccwatch/ask/, сессии его передаёт ccwatch-hook.js. При выходе недоставленные запросы убираются.
 *
 * Запуск: node ccwatch.js [--once] [--session <имя|id>] [--bell] [--no-ask]
 */
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

const ROOT = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const SESSIONS_DIR = path.join(ROOT, 'sessions');
const PROJECTS_DIR = path.join(ROOT, 'projects');
const BASE = process.env.CCWATCH_DIR || path.join(os.homedir(), '.ccwatch');
const ASK_DIR = path.join(BASE, 'ask'); // запросы плана, ждущие доставки
const DONE_DIR = path.join(BASE, 'done'); // отметки хука о доставке
const PLANS_DIR = path.join(BASE, 'plans'); // чек-листы по первой версии запроса: сессии, получившие её, ведут план там

const argv = process.argv.slice(2);
const has = (f) => argv.includes(f);
const val = (f) => { const i = argv.indexOf(f); return i >= 0 ? argv[i + 1] : undefined; };

if (has('-h') || has('--help')) {
  console.log(`ccwatch — монитор сессий Claude Code

  node ccwatch.js              живой режим
  node ccwatch.js --once       один снимок в консоль
  node ccwatch.js --session X  сразу открыть сессию (часть имени или id); с --plan — сразу весь её план
  node ccwatch.js --bell       звуковой сигнал, когда сессия закончила работу
  node ccwatch.js --no-ask     не просить план у сессий самому

Пока монитор открыт, он сам просит план с оценками времени у сессий, которые работают без плана.
  node ccwatch.js --session X --tips   сразу открыть её советы (--skills — историю скиллов)

Клавиши: в списке ↑↓ выбор и Enter открыть; в карточке Enter — весь план (↑↓ / PgUp / PgDn прокрутка),
s — советы по экономии токенов и скорости (Enter — отправить выбранный агенту, a — все сразу; там же отправленные, ответ агента и эффект;
у советов «для вас» Enter запускает их скрипт: показывает шаги правки ~/.claude/settings.json и выполняет после y, повторный запуск возвращает;
u — тот же скрипт у совета, который можно и отправить агенту);
i — история скиллов: какие, сколько раз и кем вызваны; m — написать сессии своё сообщение;
p — запросить план ещё раз; x — завершить CLI-сессию (с подтверждением); Esc — назад; q — выход
Запрос плана доходит до сессии через хук ccwatch-hook.js (как подключить, написано в его шапке).`);
  process.exit(0);
}

const TTY = !!process.stdout.isTTY && !!process.stdin.isTTY;
const ONCE = has('--once') || !TTY;
const BELL = has('--bell');
const AUTO_ASK = !has('--no-ask');
const COLOR = !!process.stdout.isTTY && !process.env.NO_COLOR;

// ── оформление ──────────────────────────────────────────────────────────────

const fg = (r, g, b) => (s) => (COLOR ? `\x1b[38;2;${r};${g};${b}m${s}\x1b[39m` : String(s));
const C = {
  pink: fg(255, 79, 163),
  cyan: fg(54, 226, 255),
  green: fg(90, 225, 150),
  amber: fg(255, 190, 90),
  text: fg(230, 230, 245),
  dim: fg(125, 122, 150),
  faint: fg(72, 70, 92),
};
const bold = (s) => (COLOR ? `\x1b[1m${s}\x1b[22m` : s);
const PINK = [255, 79, 163];
const CYAN = [54, 226, 255];
const SPIN = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
const EIGHTHS = ['', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

const chars = (s) => Array.from(String(s));
const vis = (s) => chars(String(s).replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '')).length;
// управляющие символы из чужого текста (табы, \f в путях) ломают отрисовку — заменяем пробелом
const plain = (s) => String(s).replace(/[\x00-\x1a\x1c-\x1f\x7f]/g, ' ');
const clip = (s, w) => { const a = chars(plain(s)); return w <= 0 ? '' : a.length <= w ? a.join('') : a.slice(0, w - 1).join('') + '…'; };
const pad = (s, w) => { const c = clip(s, w); return c + ' '.repeat(Math.max(0, w - vis(c))); };
const spread = (left, right, w) => left + ' '.repeat(Math.max(1, w - vis(left) - vis(right))) + right;

// обрезает уже раскрашенную строку по видимой ширине, чтобы она не переносилась
function clipAnsi(s, w) {
  if (vis(s) <= w) return s;
  let out = '';
  let n = 0;
  for (const [t] of String(s).matchAll(/\x1b\[[0-9;?]*[A-Za-z]|./gsu)) {
    if (t.length > 1 && t[0] === '\x1b') { out += t; continue; }
    if (n >= w - 1) { out += '…'; break; }
    out += t;
    n++;
  }
  return out + (COLOR ? '\x1b[0m' : '');
}

function bar(frac, w) {
  frac = Math.min(1, Math.max(0, frac || 0));
  const cells = frac * w;
  const full = Math.floor(cells);
  const part = Math.floor((cells - full) * 8);
  let s = '';
  for (let i = 0; i < w; i++) {
    const t = w > 1 ? i / (w - 1) : 0;
    const paint = fg(...PINK.map((v, k) => Math.round(v + (CYAN[k] - v) * t)));
    if (i < full) s += paint('█');
    else if (i === full && part > 0) s += paint(EIGHTHS[part]);
    else s += C.faint('─');
  }
  return s;
}

function tok(n) {
  if (!n) return '0';
  if (n < 1e3) return String(Math.round(n));
  if (n < 1e6) return (n / 1e3).toFixed(n < 1e4 ? 1 : 0) + 'k';
  if (n < 1e9) return (n / 1e6).toFixed(n < 1e7 ? 2 : 1) + 'M';
  return (n / 1e9).toFixed(2) + 'B';
}

function ago(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return s + 'с';
  const m = Math.floor(s / 60);
  if (m < 60) return m + 'м ' + String(s % 60).padStart(2, '0') + 'с';
  const h = Math.floor(m / 60);
  return h + 'ч ' + String(m % 60).padStart(2, '0') + 'м';
}

const shortModel = (m) => (m || '?').replace(/^claude-/, '').replace(/-\d{8}$/, '');
const entryName = (e) => ({ 'claude-desktop': 'десктоп', cli: 'cli' })[e] || e;

// ── чтение транскриптов ─────────────────────────────────────────────────────

const MARK = Buffer.from('"type":"a'); // начало "type":"assistant" и "type":"ai-title"
const TASK_MARK = Buffer.from('"toolUseResult":{"task":{'); // ответ TaskCreate с номером задачи
const RESULT_MARK = Buffer.from('"type":"tool_result"'); // вывод инструмента; перед ним стоит "tool_use_id"
const TUR_MARK = Buffer.from('"toolUseResult"'); // копия вывода для интерфейса — её не считаем
const IMG_MARK = Buffer.from('"type":"image"');
const TIP_MARK = Buffer.from('(ccwatch:'); // метка совета в тексте, который хук доставил сессии
const TS_MARK = Buffer.from('"timestamp":"');
const CMD_MARK = Buffer.from('<command-name>'); // команда, набранная через «/»
const USER_MARK = Buffer.from('"type":"user"');
// встроенные команды Claude Code — не скиллы
const BUILTIN = new Set(('add-dir agents background btw bug clear compact config context cost doctor effort exit export fast feedback help hooks ide '
  + 'init install-github-app login logout mcp memory model output-style permissions plugin plugins pr-comments privacy-settings rc '
  + 'release-notes remote-control resume review rewind sandbox status statusline tasks terminal-setup theme todos upgrade usage vim workflows').split(' '));
// ожидание в цикле: until/while/for со sleep или голый sleep (в PowerShell — с Start-Sleep)
const POLL_RE = /\b(until|while|for|do)\b[\s\S]*\bsleep\b|^\s*(Start-)?sleep\s+(-\w+\s+)?\d/i;
const ATTACH_MARK = Buffer.from('"type":"attachment"'); // доставка хука лежит в строке-вложении с временем
const QUEUE_MARK = Buffer.from('"type":"queue-operation"'); // сообщение или уведомление из очереди
const QUEUED_MARK = Buffer.from('"attachment":{"type":"queued_command"'); // его время — когда набрали, а не когда отдали модели
// прогон проверки: тестовый набор, а не просмотр файла с таким именем
const CHECK_RE = /\b(vitest|pytest|jest|mocha|checkpoint|suite)\b|\bplaywright\s+test\b|\b(cargo|go|dotnet)\s+test\b|\btest:\w+|\b(yarn|npm|pnpm|bun)(\s+run)?\s+test\b/i;
// команда, после которой код уже другой: git-операции, запись в файлы, скрипт из heredoc, установка зависимостей
const CHANGE_RE = /\bgit\s+(commit|merge|checkout|switch|reset|rebase|pull|cherry-pick|stash|apply|am|restore)\b|\bsed\s+-i|\btee\b|<<|\b(python3?|py|node)\s+-[ce]?\s|\b(cp|mv|rm|patch)\s|(^|[^0-9&>])>(?!&|\s*\/dev\/null)|\b(yarn|npm|pnpm)\s+(install|add|ci)\b|\b(Set|Add)-Content\b|\bOut-File\b|\b(Copy|Move|Remove)-Item\b/i;
const EDIT_TOOLS = /^(Edit|Write|MultiEdit|NotebookEdit)$/;
const NOTE_FILE = /\.(md|txt|log)$|[\\/](Temp|tmp)[\\/]|scratchpad/i; // заметки и черновики — не правка кода
// коммит в выводе проверки: «front.unit: pass 05a7d65bbeab» или «HEAD 05a7d65»
const RUN_HASH = /([\w.\/-]+):\s+(?:pass|passed|ok|fail|failed|error|skip|skipped|cached)\w*\s+([0-9a-f]{7,40})\b/gi;
const HEAD_HASH = /\b(?:HEAD|commit|sha|коммит|хеш)\b[\s:=@]+([0-9a-f]{7,40})\b/i;
const CHECK_ROLE = /test|check|qa\b|review|verif|провер|прогон|ревью|аудит/i; // проверяющий — по типу агента или заданию
const PAUSE = 30 * 60e3; // перерыв дольше — агент не работал: ждал или получил позднее уведомление
const MODEL_CAP = 10 * 60e3; // «ответ» дольше — это пауза, а не работа модели
const RAMP_OK = 3 * 60e3; // нормальный разгон субагента: прочитать задание и нужные файлы
const SHELL_LIMIT = 120e3; // тайм-аут команды по умолчанию; больше 10 минут поставить нельзя
const IMG_TOKENS = 1600; // картинка в контексте — около 1,6k токенов, сколько бы ни весил base64
const BYTES_PER_TOKEN = 3.3; // грубо: смесь кода, английского и русского
// Цена относительно обычного ввода (одинаково у всех моделей Claude). Советы ранжируем по ней:
// чтение кэша — почти все токены, но запись в кэш при ~4% токенов даёт ~40% денег.
const PRICE = { read: 0.1, write5m: 1.25, write1h: 2, input: 1, output: 5 };
const COMPACT_AT = 400e3; // окно автосжатия, которое предлагает совет про большой контекст (настройка — от 100k до 1M)

// делит команду по && ; и переводам строк, не заглядывая внутрь кавычек
function splitTop(c) {
  const parts = [];
  let cur = '';
  let q = null;
  for (let i = 0; i < c.length; i++) {
    const ch = c[i];
    if (q) { cur += ch; if (ch === q && c[i - 1] !== '\\') q = null; continue; }
    if (ch === '"' || ch === "'") { q = ch; cur += ch; continue; }
    if (ch === '\n' || ch === ';' || (ch === '&' && c[i + 1] === '&')) { if (ch === '&') i++; parts.push(cur); cur = ''; continue; }
    cur += ch;
  }
  parts.push(cur);
  return parts.map((p) => p.replace(/\\$/, '').trim()).filter(Boolean);
}

// суть команды: без «cd …», «mkdir …», присваиваний переменных, тела heredoc и длинных путей (остаётся имя файла)
function cmdCore(c) {
  const parts = splitTop(String(c).replace(/<<-?\s*(['"]?)(\w+)\1[\s\S]*$/, '<<$2 …'))
    .map((p) => p.replace(/^\(+\s*/, '').replace(/\)+$/, '').trim()); // подоболочка «( … )» — по сути та же команда
  const noise = (p) => /^(cd|mkdir|export|set|pushd|popd)\b/.test(p) || /^\w+=("[^"]*"|'[^']*'|\S*)$/.test(p);
  const keep = parts.filter((p) => !noise(p));
  return (keep.length ? keep : parts).join(' && ')
    .replace(/^time\s+/, '')
    .replace(/(?:[A-Za-z]:)?(?:[\\/][^\s"'\\/]+){3,}/g, (m) => '…/' + m.split(/[\\/]/).slice(-2).join('/'))
    .replace(/\s+/g, ' ').trim();
}

// Чем считать повтором: та же программа или скрипт с любыми параметрами («node tools/sim/run.mjs»,
// «npm run test»). Просмотр (ls, grep, git log…) и одноразовые скрипты (node -e, heredoc) — не повторы.
const TRIVIAL = /^(ls|cat|grep|rg|find|head|tail|wc|echo|sed|awk|pwd|cp|mv|rm|touch|stat|du|tree|sort|uniq|diff|which|where|type|true|printf|sleep|test|\[|for|while|if|do|done|then|case|Get-ChildItem|Get-Content|Select-String)$/;
const RUNNERS = /^(node|python3?|py|bash|sh|npx|bun|deno|tsx|ts-node|pwsh|powershell)$/;
function cmdHead(core) {
  let t = core.split(' ').filter(Boolean);
  while (t.length && (/^(time|env|nice|nohup|sudo)$/.test(t[0]) || /^\w+=/.test(t[0]))) t = t.slice(1);
  if (t[0] === 'timeout') t = t.slice(t[1] && /^\d/.test(t[1]) ? 2 : 1);
  const prog = (t[0] || '').replace(/^.*[\\/]/, '');
  if (!prog || TRIVIAL.test(prog)) return '';
  const args = t.slice(1).filter((x) => !x.startsWith('-') && !x.startsWith('<<'));
  if (prog === 'git') return /^(status|log|diff|show|branch|rev-parse|ls-files)$/.test(args[0] || '') ? '' : 'git ' + (args[0] || '');
  if (/^(npm|pnpm|yarn)$/.test(prog)) return [prog].concat(args.slice(0, 2)).join(' ');
  if (RUNNERS.test(prog)) return /^-(e|c)$/.test(t[1] || '') || t[1] === '-' || !args.length ? '' : prog + ' ' + args[0];
  return prog;
}

// имя программы в команде: без обёрток time / env / timeout N / VAR=…
function progName(core) {
  let t = core.split(' ').filter(Boolean);
  while (t.length && (/^(time|env|nice|nohup|sudo)$/.test(t[0]) || /^\w+=/.test(t[0]))) t = t.slice(1);
  if (t[0] === 'timeout') t = t.slice(t[1] && /^\d/.test(t[1]) ? 2 : 1);
  return (t[0] || '').replace(/^.*[\\/]/, '');
}

function callKey(b) {
  const i = b.input || {};
  if (b.name === 'Read' && i.file_path) return 'read:' + String(i.file_path).replace(/\\/g, '/').toLowerCase();
  if ((b.name === 'Bash' || b.name === 'PowerShell') && i.command) { const h = cmdHead(cmdCore(i.command)); return h ? 'cmd:' + h : ''; }
  return '';
}

function callLabel(b) {
  const i = b.input || {};
  if (b.name === 'Read' && i.file_path) return path.basename(String(i.file_path));
  if (i.command) return cmdCore(i.command);
  return toolName(b.name) + (i.description ? ': ' + i.description : '');
}

// набор проверки: команда с программы-прогонщика («vitest run …», «checkpoint.py run front.unit»), без окружения и путей
function suiteName(core) {
  const m = CHECK_RE.exec(core);
  return m ? core.slice(core.lastIndexOf(' ', m.index) + 1).replace(/^\S*[\\/]/, '') : core;
}

// русское склонение: plural(3, 'раз', 'раза', 'раз') → 'раза'
function plural(n, one, few, many) {
  const a = Math.abs(n) % 100;
  const b = a % 10;
  if (a > 10 && a < 20) return many;
  if (b === 1) return one;
  return b >= 2 && b <= 4 ? few : many;
}
const times = (n) => `${n} ${plural(n, 'раз', 'раза', 'раз')}`;

// Транскрипт дочитывается с места, где остановились в прошлый раз: большие файлы не перечитываются.
class Transcript {
  constructor(file) {
    this.file = file;
    this.offset = 0;
    this.mtime = 0;
    this.reset();
  }

  reset() {
    this.offset = 0;
    this.first = 0; // время первой и последней реплики агента
    this.lastAt = 0;
    this.model = '';
    this.last = null; // последний инструмент: {tool, detail, at}
    this.usage = new Map(); // message.id → {i, c, o, m}: одно сообщение пишется несколькими строками
    this.byModel = new Map(); // модель → {i — вход, wi — он же по цене, c — из кэша, o — выход}
    // для советов по экономии
    this.calls = []; // вызовы инструментов: {id, key, label, t, ctx — размер контекста запроса}
    this.results = new Map(); // tool_use id → примерный размер вывода в токенах
    this.reqs = []; // запросы к модели: {t, ctx}
    this.compactions = []; // когда контекст резко сжимался (/compact)
    this.rewrites = 0; // запросы, где кэш пришлось записать почти целиком
    this.rewriteTok = 0;
    this.rewrite5m = 0; // из них записано с 5-минутным сроком жизни (у субагентов это по умолчанию)
    this.rewriteW = 0; // те же записи по цене ввода: 5 минут ×1,25, час ×2
    this.maxCtx = 0;
    this.bigReads = 0; // чтение кэша в запросах, где контекст больше 200k
    this.deliveries = []; // доставленные советы: {key, t}
    // для советов по скорости
    this.resultAt = new Map(); // tool_use id → когда пришёл результат
    this.reqById = new Map(); // message.id → запрос из reqs
    this.thinkMs = 0; // сколько модель размышляла
    this.thinkBy = new Map(); // «модель effort» → мс размышлений: какую настройку менять
    this.effort = ''; // уровень effort из транскрипта
    this.skills = []; // скиллы: {name, t, args, by: 'agent' — инструмент Skill, 'user' — набрано через /}
    // после resume в журнал заново попадают старые строки со старым временем — их пропускаем
    this.seen = new Set(); // uuid разобранных строк
    this.maxAt = 0; // самое позднее время среди них
    this.skipped = 0; // сколько повторов пропущено
    this.marks = new Set(); // уже учтённые команды через «/» и доставки советов: «имя@время»
    // активное время: куски работы без перерывов дольше PAUSE
    this.startAt = 0; // самый ранний ввод (у субагента — его задание)
    this.chunkAt = 0; // начало текущего куска
    this.activeMs = 0; // сумма закрытых кусков
    this.inputs = []; // когда модель получала ввод: сообщение, уведомление, вложение (результаты — в resultAt)
    this.byId = new Map(); // tool_use id → вызов из calls
    this.ver = 0; // сколько раз агент менял код: правки, запись в файлы, git-операции
    this.rampMs = -1; // активное время до первой правки кода; -1 — правок не было
    this.modelCache = null;
  }

  // Команды, набранные человеком через «/» (среди них и скиллы): строка пользователя с <command-name>.
  scanCommands(buf, end) {
    let pos = 0;
    while ((pos = buf.indexOf(CMD_MARK, pos)) !== -1 && pos < end) {
      const from = buf.lastIndexOf(10, pos) + 1;
      let to = buf.indexOf(10, pos);
      if (to < 0 || to > end) to = end;
      const head = buf.toString('utf8', pos, Math.min(to, pos + 300));
      const user = buf.indexOf(USER_MARK, from);
      const ts = buf.lastIndexOf(TS_MARK, to);
      pos = to + 1;
      if (user < 0 || user > to || ts < from) continue;
      const m = /<command-name>\/?([^<]+)<\/command-name>/.exec(head);
      const a = /<command-args>([^<]*)<\/command-args>/.exec(buf.toString('utf8', from, Math.min(to, from + 4000)));
      const t = Date.parse(buf.toString('latin1', ts + TS_MARK.length, ts + TS_MARK.length + 24));
      if (m && t && this.fresh('cmd:' + m[1].trim() + '@' + t)) this.skills.push({ name: m[1].trim(), args: a ? a[1].trim() : '', t, by: 'user' });
    }
  }

  // строку с тем же именем и временем уже учли: это её копия после resume
  fresh(mark) {
    if (this.marks.has(mark)) return false;
    this.marks.add(mark);
    return true;
  }

  // Доставленные хуком советы: строка-вложение с меткой «(ccwatch:<совет>)». Та же метка встречается
  // и в служебных строках запроса, но без времени — их пропускаем.
  scanDeliveries(buf, end) {
    let pos = 0;
    while ((pos = buf.indexOf(TIP_MARK, pos)) !== -1 && pos < end) {
      const from = buf.lastIndexOf(10, pos) + 1;
      let to = buf.indexOf(10, pos);
      if (to < 0 || to > end) to = end;
      pos = to + 1;
      const a = buf.indexOf(ATTACH_MARK, from);
      if (a < 0 || a > to || to - from > 500e3) continue;
      const line = buf.toString('utf8', from, to);
      const ts = /"timestamp":"([^"]+)"/.exec(line);
      const t = ts ? Date.parse(ts[1]) : 0;
      if (!t) continue;
      for (const key of new Set([...line.matchAll(/\(ccwatch:(\w+)\)/g)].map((m) => m[1]))) if (this.fresh('tip:' + key + '@' + t)) this.deliveries.push({ key, t });
    }
  }

  // Размеры выводов инструментов считаем по байтам, не разбирая JSON: строки с выводами — основная
  // масса транскрипта (у субагентов это сотни мегабайт), а точность до токена тут не нужна.
  scanResults(buf, end) {
    let pos = 0;
    while ((pos = buf.indexOf(RESULT_MARK, pos)) !== -1 && pos < end) {
      let lineEnd = buf.indexOf(10, pos);
      if (lineEnd < 0 || lineEnd > end) lineEnd = end;
      let next = buf.indexOf(RESULT_MARK, pos + RESULT_MARK.length);
      if (next < 0 || next > lineEnd) next = lineEnd;
      let segEnd = buf.indexOf(TUR_MARK, pos);
      if (segEnd < 0 || segEnd > next) segEnd = next;
      const id = /"tool_use_id":"([^"]+)"/.exec(buf.toString('latin1', Math.max(0, pos - 100), pos));
      let images = 0;
      for (let ip = pos; (ip = buf.indexOf(IMG_MARK, ip)) !== -1 && ip < segEnd; ip += IMG_MARK.length) images++;
      if (id) this.results.set(id[1], images ? images * IMG_TOKENS : Math.round((segEnd - pos) / BYTES_PER_TOKEN));
      // время результата — для длительности вызова; верхнее поле строки, оно идёт в её конце
      const tsPos = buf.lastIndexOf(TS_MARK, lineEnd);
      if (id && tsPos > buf.lastIndexOf(10, pos)) {
        const t = Date.parse(buf.toString('latin1', tsPos + TS_MARK.length, tsPos + TS_MARK.length + 24));
        if (t) this.resultAt.set(id[1], t);
      }
      const c = id && this.byId.get(id[1]);
      if (c && c.shell) this.readOutput(c, buf.toString('utf8', pos, Math.min(segEnd, pos + (c.check ? 6000 : 600))));
      pos = next < lineEnd ? next : lineEnd + 1;
    }
  }

  // Начало вывода команды: упёрлась ли она в тайм-аут и на каком коммите шла проверка.
  // Проверки, которые печатают «набор: pass <хеш>», сравниваем по каждому набору; иначе — по «HEAD <хеш>».
  readOutput(c, head) {
    const text = head.replace(/\\[nrt]/g, ' ').replace(/\\"/g, '"');
    if (/\bCommand timed out after\b/.test(text.slice(0, 500))) c.timedOut = true; // «timed out» в выводе тестов — не то
    if (!c.check) return;
    const keys = new Set();
    for (const m of text.matchAll(RUN_HASH)) keys.add(m[1] + '@' + m[2].slice(0, 7).toLowerCase());
    const h = !keys.size && HEAD_HASH.exec(text);
    if (h) keys.add(c.suite + '@' + h[1].slice(0, 7).toLowerCase());
    if (keys.size) c.hashes = [...keys];
  }

  // Вводы модели: сообщения, уведомления, взятые из очереди, и вложения (вывод хуков, напоминания) —
  // с них начинается запрос. Результаты инструментов собирает scanResults. Постановка в очередь
  // (enqueue и вложение queued_command) — время набора, модель его ещё не получила.
  scanInputs(buf, end) {
    const done = new Set(); // строка с несколькими метками — одна
    for (const mark of [ATTACH_MARK, QUEUE_MARK, USER_MARK]) {
      let pos = 0;
      while ((pos = buf.indexOf(mark, pos)) !== -1 && pos < end) {
        // результат инструмента виден сразу за меткой — такие строки (бывают в мегабайты) не размечаем
        if (mark === USER_MARK && buf.subarray(pos, pos + 400).indexOf(RESULT_MARK) >= 0) { pos += mark.length; continue; }
        const from = buf.lastIndexOf(10, pos) + 1;
        let to = buf.indexOf(10, pos);
        if (to < 0 || to > end) to = end;
        const at = pos;
        pos = to + 1;
        if (done.has(from)) continue;
        done.add(from);
        if (mark === ATTACH_MARK && buf.subarray(from, Math.min(at, from + 300)).indexOf(QUEUED_MARK) >= 0) continue;
        if (mark === QUEUE_MARK && buf.toString('latin1', at + mark.length, at + mark.length + 24).includes('enqueue')) continue;
        const ts = buf.lastIndexOf(TS_MARK, to);
        if (ts < from) continue;
        const t = Date.parse(buf.toString('latin1', ts + TS_MARK.length, ts + TS_MARK.length + 24));
        if (!t) continue;
        this.inputs.push(t);
        if (!this.startAt || t < this.startAt) this.startAt = t;
      }
    }
  }

  pull(onLine, withTasks) {
    let st;
    try { st = fs.statSync(this.file); } catch { return; }
    this.mtime = st.mtimeMs;
    if (st.size < this.offset) this.reset(); // файл переписали — читаем заново
    if (st.size === this.offset) return;
    const fd = fs.openSync(this.file, 'r');
    try {
      let chunk = 4 << 20;
      while (this.offset < st.size) {
        const len = Math.min(chunk, st.size - this.offset);
        const buf = Buffer.allocUnsafe(len);
        const n = fs.readSync(fd, buf, 0, len, this.offset);
        const end = n > 0 ? buf.lastIndexOf(10, n - 1) : -1;
        if (end < 0) { // строка длиннее куска или ещё дописывается
          if (this.offset + n >= st.size) break;
          chunk *= 2;
          continue;
        }
        this.scanInputs(buf, end); // первым: начало работы агента нужно уже первой реплике
        for (const [from, to] of findLines(buf, end, withTasks)) {
          let d;
          try { d = JSON.parse(buf.toString('utf8', from, to)); } catch { continue; }
          if (this.repeated(d)) continue;
          this.take(d);
          if (onLine) onLine(d);
        }
        this.scanResults(buf, end);
        if (withTasks) this.scanDeliveries(buf, end); // главный транскрипт: туда хук и доставляет
        if (withTasks) this.scanCommands(buf, end); // «/» набирает только человек — в главной сессии
        this.offset += end + 1;
      }
    } finally {
      fs.closeSync(fd);
    }
  }

  // Копия строки после resume: uuid уже встречался или время раньше уже прочитанного (с запасом в минуту).
  // Без этого вызовы, скиллы и шаги плана из старой части журнала считались бы второй раз.
  repeated(d) {
    if (d.uuid) {
      if (this.seen.has(d.uuid)) { this.skipped++; return true; }
      this.seen.add(d.uuid);
    }
    const t = Date.parse(d.timestamp) || 0;
    if (t && t < this.maxAt - 60e3) { this.skipped++; return true; }
    if (t > this.maxAt) this.maxAt = t;
    return false;
  }

  // Активное время идёт кусками: перерыв дольше PAUSE не считаем — иначе позднее уведомление
  // субагенту через несколько часов добавляло бы эти часы к его работе.
  mark(at) {
    if (!this.chunkAt) this.chunkAt = this.startAt && this.startAt <= at && at - this.startAt <= PAUSE ? this.startAt : at;
    else if (at - this.lastAt > PAUSE) { this.activeMs += this.lastAt - this.chunkAt; this.chunkAt = at; }
  }

  // активное время к моменту at
  activeAt(at) {
    return this.chunkAt ? this.activeMs + Math.max(0, at - this.chunkAt) : 0;
  }

  // Время модели: от ввода (результат инструмента, сообщение, уведомление, вложение) до конца ответа.
  // Ввод раньше конца прошлого ответа — не этого запроса: тогда считаем только сам ответ;
  // дольше MODEL_CAP — это была пауза, а не работа модели, и тоже считаем только ответ.
  modelMs() {
    const key = `${this.reqs.length}:${this.inputs.length}:${this.resultAt.size}:${this.lastAt}`;
    if (this.modelCache && this.modelCache.key === key) return this.modelCache.ms;
    const inp = this.inputs.concat([...this.resultAt.values()]).sort((a, b) => a - b);
    let prevEnd = 0;
    let ms = 0;
    for (const r of this.reqs) {
      const i = countBefore(inp, r.t + 1) - 1;
      const from = i >= 0 && inp[i] > prevEnd ? inp[i] : r.t;
      const d = Math.max(0, r.end - from);
      ms += d <= MODEL_CAP ? d : Math.min(MODEL_CAP, Math.max(0, r.end - r.t));
      prevEnd = Math.max(prevEnd, r.end);
    }
    this.modelCache = { key, ms };
    return ms;
  }

  take(d) {
    if (d.type !== 'assistant' || !d.message) return;
    const m = d.message;
    const at = Date.parse(d.timestamp) || 0;
    if (at) this.mark(at);
    if (at && (!this.first || at < this.first)) this.first = at;
    if (at > this.lastAt) this.lastAt = at;
    if (m.model && !m.model.startsWith('<')) this.model = m.model;
    const u = m.usage;
    const ctx = u ? (u.input_tokens || 0) + (u.cache_creation_input_tokens || 0) + (u.cache_read_input_tokens || 0) : 0;
    if (d.effort) this.effort = d.effort;
    if (d.thinkingDurationMs) {
      this.thinkMs += d.thinkingDurationMs;
      const by = (this.model || '?') + ' ' + (this.effort || '?');
      this.thinkBy.set(by, (this.thinkBy.get(by) || 0) + d.thinkingDurationMs);
    }
    let tools = 0;
    for (const b of Array.isArray(m.content) ? m.content : []) {
      if (!b || b.type !== 'tool_use') continue;
      tools++;
      const i = b.input || {};
      if (b.name === 'Skill') this.skills.push({ name: String(i.skill || i.name || '?'), args: String(i.args || ''), t: at, by: 'agent' });
      const shell = b.name === 'Bash' || b.name === 'PowerShell';
      const cmd = shell ? String(i.command || '') : '';
      const core = shell ? cmdCore(cmd) : '';
      const prog = progName(core);
      const poll = shell && POLL_RE.test(cmd);
      const check = shell && !poll && !TRIVIAL.test(prog) && CHECK_RE.test(core); // core — без тела heredoc
      // правка кода (не заметок) или команда, меняющая файлы, — дальше проверки идут уже на другом коде
      const edit = EDIT_TOOLS.test(b.name) && !NOTE_FILE.test(String(i.file_path || i.notebook_path || ''));
      if (edit || (shell && !check && CHANGE_RE.test(cmd))) this.ver++;
      if (edit && this.rampMs < 0) this.rampMs = this.activeAt(at);
      const c = {
        id: b.id, key: callKey(b), label: callLabel(b), shell, prog, poll, t: at, ctx,
        check, suite: check ? suiteName(core) : '', ver: this.ver, limit: shell ? Number(i.timeout) || 0 : 0, bg: !!i.run_in_background,
      };
      this.calls.push(c);
      this.byId.set(b.id, c);
    }
    if (!u || !m.id) return;
    // wi — вход по цене: обычный ×1, запись в кэш на 5 минут ×1,25, на час ×2
    const cc = u.cache_creation || {};
    const w1h = cc.ephemeral_1h_input_tokens || 0;
    const w5m = cc.ephemeral_5m_input_tokens != null ? cc.ephemeral_5m_input_tokens : (u.cache_creation_input_tokens || 0) - w1h;
    const next = {
      i: (u.input_tokens || 0) + (u.cache_creation_input_tokens || 0),
      wi: (u.input_tokens || 0) * PRICE.input + w5m * PRICE.write5m + w1h * PRICE.write1h,
      c: u.cache_read_input_tokens || 0,
      o: u.output_tokens || 0,
      m: this.model || '?',
    };
    const prev = this.usage.get(m.id);
    if (prev) this.bump(prev, -1);
    else if (ctx) this.reqById.set(m.id, this.request(at, ctx, u));
    this.usage.set(m.id, next);
    this.bump(next, 1);
    const req = this.reqById.get(m.id);
    if (req) { req.end = Math.max(req.end, at); req.tools += tools; }
  }

  // новый запрос к модели: следим за сжатием контекста, полной перезаписью кэша и раздутым контекстом
  request(at, ctx, u) {
    const last = this.reqs[this.reqs.length - 1];
    const written = u.cache_creation_input_tokens || 0;
    if (last && last.ctx > 50e3 && ctx < 0.6 * last.ctx) this.compactions.push(at);
    // первый запрос всегда пишет кэш с нуля, это не потеря
    if (last && ctx > 50e3 && written > 0.5 * ctx) {
      const cc = u.cache_creation || {};
      const h1 = cc.ephemeral_1h_input_tokens || 0;
      const m5 = cc.ephemeral_5m_input_tokens != null ? cc.ephemeral_5m_input_tokens : written - h1;
      this.rewrites++;
      this.rewriteTok += written;
      this.rewrite5m += m5;
      this.rewriteW += m5 * PRICE.write5m + h1 * PRICE.write1h;
    }
    if (ctx > this.maxCtx) this.maxCtx = ctx;
    if (ctx > 200e3) this.bigReads += u.cache_read_input_tokens || 0;
    const req = { t: at, ctx, end: at, tools: 0 };
    this.reqs.push(req);
    return req;
  }

  bump(u, k) {
    let b = this.byModel.get(u.m);
    if (!b) this.byModel.set(u.m, (b = { i: 0, wi: 0, c: 0, o: 0 }));
    b.i += k * u.i;
    b.wi += k * u.wi;
    b.c += k * u.c;
    b.o += k * u.o;
  }

  get totals() {
    const t = { i: 0, wi: 0, c: 0, o: 0 };
    for (const b of this.byModel.values()) { t.i += b.i; t.wi += b.wi; t.c += b.c; t.o += b.o; }
    return t;
  }
}

// Строки ищем прямо в байтах: в транскрипте бывают многомегабайтные служебные строки,
// и декодировать весь файл в текст в разы дольше, чем найти нужные. Возвращает
// [начало, конец] нужных строк в порядке их следования.
function findLines(buf, end, withTasks) {
  const found = new Map();
  const grab = (mark, ok) => {
    let pos = 0;
    while ((pos = buf.indexOf(mark, pos)) !== -1 && pos < end) {
      if (ok && !ok(buf.toString('latin1', pos + mark.length, pos + mark.length + 9))) { pos += mark.length; continue; }
      const from = buf.lastIndexOf(10, pos) + 1;
      let to = buf.indexOf(10, pos);
      if (to < 0 || to > end) to = end;
      found.set(from, to);
      pos = to + 1;
    }
  };
  grab(MARK, (k) => k === 'ssistant"' || k.startsWith('i-title"'));
  if (withTasks) grab(TASK_MARK);
  return [...found].sort((a, b) => a[0] - b[0]);
}

// оценка времени в конце шага: «~15м», «~1ч 30м», «(~2h)»
const EST = /\s*[(\[]?\s*[~≈]\s*(\d+(?:[.,]\d+)?)\s*(ч|час\S*|h|hr|м|мин\S*|m|min)\.?(?:\s*(\d+)\s*(м|мин\S*|m|min)\.?)?\s*[)\]]?\s*$/i;

function step(content, status, id) {
  let text = String(content ?? '').replace(/\s+/g, ' ').trim();
  let est = 0;
  const m = EST.exec(text);
  if (m) {
    const n = parseFloat(m[1].replace(',', '.'));
    est = (/^(ч|h)/i.test(m[2]) ? n * 60 : n) * 60e3 + (m[3] ? Number(m[3]) * 60e3 : 0);
    text = text.slice(0, m.index).replace(/[\s—–-]+$/, '').trim();
  }
  return { content: text, status: status || 'pending', id, est, startedAt: 0, doneAt: 0 };
}

// строка плана из текста ответа: «1. ✓ Шаг ~15м», жирный markdown убираем
function textStep(s) {
  let t = s.replace(/\*\*/g, '').trim();
  let status = 'pending';
  if (/^[✓✔]/.test(t)) { status = 'completed'; t = t.replace(/^[✓✔]\s*/, ''); }
  else if (/^▶/.test(t)) { status = 'in_progress'; t = t.replace(/^▶\s*/, ''); }
  return step(t, status);
}

function dur(ms) {
  const m = Math.round(ms / 60e3);
  if (m < 1) return '<1м';
  if (m < 60) return m + 'м';
  const h = Math.floor(m / 60);
  return h + 'ч' + (m % 60 ? ' ' + (m % 60) + 'м' : '');
}

// оценка с тильдой: «~15м», но «<1м» без неё
const approx = (ms) => { const d = dur(ms); return d.startsWith('<') ? d : '~' + d; };

// время окончания; если это не сегодня — с датой, иначе «04:57» при остатке в сутки сбивает с толку
function clock(t) {
  const d = new Date(t);
  const hm = d.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
  return d.toDateString() === new Date().toDateString() ? hm : d.toLocaleDateString('ru-RU', { day: '2-digit', month: '2-digit' }) + ' ' + hm;
}

// update_plan десктоп-приложения: полный список items или сдвиги advance / done / start.
function applyPlan(plan, i) {
  if (Array.isArray(i.items)) plan = i.items.map((x) => step(x.content, x.status));
  const at = (n) => plan[n - 1];
  if (Array.isArray(i.done)) for (const n of i.done) if (at(n)) at(n).status = 'completed';
  if (i.advance) {
    const cur = plan.findIndex((x) => x.status === 'in_progress');
    if (cur >= 0) plan[cur].status = 'completed';
    let next = plan.findIndex((x, k) => k > cur && x.status === 'pending');
    if (next < 0) next = plan.findIndex((x) => x.status === 'pending');
    if (next >= 0) plan[next].status = 'in_progress';
  }
  if (i.start && at(i.start)) at(i.start).status = 'in_progress';
  return plan;
}

function toolName(n) {
  if (!n.startsWith('mcp__')) return n;
  const p = n.split('__');
  return p.slice(2).join('__') || p[1];
}

function toolDetail(i) {
  const v = i.description || (i.file_path && path.basename(String(i.file_path))) || i.command
    || i.pattern || i.query || i.url || i.prompt || i.action || '';
  return String(v).replace(/\s+/g, ' ').trim();
}

const toolUses = (d) => (d.type === 'assistant' && d.message && Array.isArray(d.message.content)
  ? d.message.content.filter((b) => b && b.type === 'tool_use')
  : []);
const action = (b, at) => ({ tool: toolName(b.name), detail: toolDetail(b.input || {}), at: Date.parse(at) || Date.now() });
const describe = (a) => a.tool + (a.detail ? ' · ' + a.detail : '');

function findTranscript(id, cwd) {
  if (cwd) {
    const guess = path.join(PROJECTS_DIR, cwd.replace(/[^a-zA-Z0-9]/g, '-'), id + '.jsonl');
    if (fs.existsSync(guess)) return guess;
  }
  let dirs = [];
  try { dirs = fs.readdirSync(PROJECTS_DIR); } catch {}
  for (const d of dirs) {
    const f = path.join(PROJECTS_DIR, d, id + '.jsonl');
    if (fs.existsSync(f)) return f;
  }
  return null;
}

// сколько элементов отсортированного массива меньше t (бинарный поиск)
function countBefore(arr, t) {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) { const mid = (lo + hi) >> 1; if (arr[mid] < t) lo = mid + 1; else hi = mid; }
  return lo;
}

const pct = (x) => Math.round(x * 100) + '%';

// итог «до / после» для меры, где меньше — лучше
function verdict(before, after, enough) {
  if (!enough || !before) return { text: 'мало данных', paint: C.faint };
  const change = (after - before) / before;
  if (change <= -0.2) return { text: `помогло (${Math.round(change * 100)}%)`, paint: C.green };
  if (change >= 0.2) return { text: `стало хуже (+${Math.round(change * 100)}%)`, paint: C.amber };
  return { text: 'без заметной разницы', paint: C.dim };
}

// просьба к агенту ответить на совет одной строкой — без отдельных действий
const ackRequest = (key) => ` В ответ добавь к тексту своего следующего сообщения одну строку «ccwatch: принято ${key} — что именно меняешь» `
  + `или «ccwatch: не применимо ${key} — почему». Отдельных действий ради ответа не делай и текущую работу не прерывай.`;

// ваше сообщение сессии (клавиша m) идёт тем же хуком, что советы, но с ключом msg…
const isMsg = (key) => /^msg[0-9a-z]+$/.test(key);

const TIP_TITLES = {
  thinking: 'Размышления',
  heavy: 'Большие выводы в контексте', shared: 'Общие файлы у субагентов', repeat: 'Повторы команд',
  bloated: 'Разросшиеся субагенты', shell: 'Механика у главного агента',
  slowcheck: 'Долгие проверки', smallshell: 'Мелкие команды оболочки', polling: 'Ожидание в цикле',
  recheck: 'Повторные проверки на том же коде', timeout: 'Команды упираются в тайм-аут', ramp: 'Субагенты долго разгоняются',
};

// время работы субагента: от задания до последней реплики, у работающего — до сейчас; перерывы дольше получаса не в счёт
const agentTime = (a, now) => a.t.activeAt(a.running ? Math.max(now, a.t.lastAt) : a.t.lastAt);

// команда упёрлась в тайм-аут: шла до своего лимита (по умолчанию 2 мин, больше 10 мин не ставится) или так сказано в выводе
const hitLimit = (c, dt) => !c.bg && (!!c.timedOut || dt >= Math.min(600e3, c.limit || SHELL_LIMIT) - 5e3);

class Session {
  constructor(id) {
    this.id = id;
    this.reg = null;
    this.live = false;
    this.main = null;
    this.subs = new Map(); // файл → Transcript
    this.metas = new Map();
    this.agentList = []; // {name, model, running, t}
    this.plan = []; // шаги: {content, status, id, est, startedAt, doneAt}
    this.planAt = 0; // когда агент последний раз трогал план
    this.planBornAt = 0; // когда появился текущий план
    this.planSource = ''; // 'tool', 'text' или 'file'
    this.planFileAt = 0;
    this.acks = new Map(); // ответы агента на советы: ключ совета → {t, ok, text}
    this.pendingTasks = new Map(); // tool_use id вызова TaskCreate → шаг, ждущий номера задачи
    this.model = '';
    this.ctx = 0;
    this.title = '';
    this.last = null; // последний инструмент главного агента: {tool, detail, at}
    this.ask = null; // запрос плана: {state: 'wait' | 'sent', at}
    this.autoAsked = false;
    this.lookupAt = 0;
    this.subsAt = 0;
  }

  refresh(now) {
    if (!this.main && now - this.lookupAt > 5000) {
      this.lookupAt = now;
      const f = findTranscript(this.id, this.reg && this.reg.cwd);
      if (f) this.main = new Transcript(f);
    }
    this.ask = readAsk(this.id);
    if (!this.main) return;
    this.main.pull((d) => this.onLine(d), true);
    this.readPlanFile();
    if (now - this.subsAt > 2000) {
      this.subsAt = now;
      this.pullSubagents();
    }
  }

  onLine(d) {
    if (d.type === 'ai-title') { if (d.aiTitle) this.title = d.aiTitle; return; }
    if (d.isSidechain) return;
    if (d.type === 'user') { this.onTaskCreated(d); return; }
    if (d.type !== 'assistant' || !d.message) return;
    const m = d.message;
    const u = m.usage;
    if (m.model && !m.model.startsWith('<')) this.model = m.model;
    if (u) {
      const ctx = (u.input_tokens || 0) + (u.cache_creation_input_tokens || 0) + (u.cache_read_input_tokens || 0);
      if (ctx) this.ctx = ctx;
    }
    for (const b of m.content || []) {
      if (b && b.type === 'text' && typeof b.text === 'string' && b.text.includes('ccwatch:')) this.onText(b.text, d.timestamp);
    }
    for (const b of toolUses(d)) this.onTool(b, d.timestamp);
  }

  onTool(b, at) {
    const i = b.input || {};
    const when = Date.parse(at) || Date.now();
    const prev = this.plan.slice();
    switch (b.name) {
      case 'mcp__ccd_turn__update_plan':
        this.plan = applyPlan(this.plan, i);
        this.planChanged(prev, when, 'tool');
        return;
      case 'mcp__ccd_turn__wrap_up':
        for (const s of this.plan) s.status = 'completed';
        this.planChanged(prev, when, this.planSource);
        return;
      case 'TodoWrite':
        if (Array.isArray(i.todos)) this.plan = i.todos.map((t) => step(t.content, t.status));
        this.planChanged(prev, when, 'tool');
        break;
      case 'TaskCreate': {
        const s = step(i.subject || i.description, 'pending');
        this.plan.push(s);
        this.pendingTasks.set(b.id, s);
        this.planChanged(prev, when, 'tool');
        break;
      }
      case 'TaskUpdate': {
        const s = this.plan.find((x) => x.id === String(i.taskId));
        if (s && i.status === 'deleted') this.plan.splice(this.plan.indexOf(s), 1);
        else if (s) {
          if (i.status) s.status = i.status;
          if (i.subject) s.content = step(i.subject).content;
        }
        this.planChanged(prev, when, 'tool');
        break;
      }
    }
    this.last = action(b, at);
  }

  // план и метки прямо в тексте ответа: строка «ccwatch: план» с шагами «1. …» и строки «ccwatch: ✓2 ▶3»
  onText(text, at) {
    const when = Date.parse(at) || Date.now();
    const prev = this.plan.slice();
    const lines = text.split(/\r?\n/).map((l) => l.replace(/`/g, '').trim());
    let changed = false;
    for (let k = 0; k < lines.length; k++) {
      if (/^ccwatch:\s*план\s*:?$/i.test(lines[k])) {
        const steps = [];
        while (k + 1 < lines.length) {
          const n = /^(\d+)[.)]\s+(.+)$/.exec(lines[k + 1]);
          if (n) steps.push(textStep(n[2]));
          else if (lines[k + 1] || steps.length) break;
          k++;
        }
        if (steps.length) { this.plan = steps; changed = true; }
        continue;
      }
      // ответ на совет: «ccwatch: принято repeat — сведу прогоны в скрипт» или «ccwatch: не применимо heavy — …»
      const ack = /^ccwatch:\s*(принято|не применимо|отклонено)\s+(\w+)\s*(?:[—–:-]\s*(.*))?$/i.exec(lines[k]);
      if (ack) { this.acks.set(ack[2], { t: when, ok: /принято/i.test(ack[1]), text: (ack[3] || '').trim() }); continue; }
      const mk = /^ccwatch:\s*((?:[✓✔▶]\s*\d+[\s,;]*)+)$/.exec(lines[k]);
      if (!mk || !this.plan.length) continue;
      for (const [, sym, n] of mk[1].matchAll(/([✓✔▶])\s*(\d+)/g)) {
        const x = this.plan[Number(n) - 1];
        if (!x) continue;
        if (sym === '▶') {
          for (const y of this.plan) if (y !== x && y.status === 'in_progress') y.status = 'completed';
          x.status = 'in_progress';
        } else x.status = 'completed';
      }
      changed = true;
    }
    if (changed) this.planChanged(prev, when, 'text');
  }

  // Чек-лист «- [x] / - [>] / - [ ]» из ~/.ccwatch/plans/<id>.md (так просила первая версия запроса).
  // Берём его, только если в транскрипте плана нет: сессия, получившая оба запроса, ведёт и то и другое.
  readPlanFile() {
    if (this.planSource === 'tool' || this.planSource === 'text') return;
    const f = path.join(PLANS_DIR, this.id + '.md');
    let st;
    try { st = fs.statSync(f); } catch { return; }
    if (st.mtimeMs === this.planFileAt) return;
    this.planFileAt = st.mtimeMs;
    const steps = [];
    let text = '';
    try { text = fs.readFileSync(f, 'utf8'); } catch {}
    for (const line of text.split(/\r?\n/)) {
      const m = /^\s*[-*+]\s*\[([ xX>~])\]\s*(.+?)\s*$/.exec(line);
      if (m) steps.push(step(m[2], m[1] === ' ' ? 'pending' : /[xX]/.test(m[1]) ? 'completed' : 'in_progress'));
    }
    if (!steps.length) return;
    const prev = this.plan.slice();
    this.plan = steps;
    this.planChanged(prev, st.mtimeMs, 'file');
  }

  // Переносит отметки времени со старой версии плана (шаги совпадают по тексту) и ставит
  // новые по смене статусов: так видно, сколько на самом деле занял каждый шаг.
  planChanged(prev, at, source) {
    const old = new Map(prev.map((x) => [x.content, x]));
    let matched = 0;
    for (const x of this.plan) {
      const o = old.get(x.content);
      if (!o) continue;
      matched++;
      if (o === x) continue;
      x.startedAt = x.startedAt || o.startedAt;
      x.doneAt = x.doneAt || o.doneAt;
      x.est = x.est || o.est;
    }
    if (!matched || !this.planBornAt) this.planBornAt = at;
    let lastDone = this.planBornAt;
    for (const x of this.plan) {
      if (x.status !== 'completed') x.doneAt = 0;
      if (x.status === 'in_progress' && !x.startedAt) x.startedAt = at;
      if (x.status === 'completed') {
        if (!x.doneAt) x.doneAt = at;
        if (!x.startedAt) x.startedAt = Math.min(lastDone, x.doneAt);
        lastDone = Math.max(lastDone, x.doneAt);
      }
    }
    this.planAt = at;
    if (source) this.planSource = source;
  }

  // оценки шагов: всего, сколько осталось, какой шаг вышел за оценку
  estimate(now) {
    const withEst = this.plan.filter((x) => x.est);
    if (!withEst.length) return null;
    let total = 0;
    let left = 0;
    let over = null;
    for (const x of withEst) {
      total += x.est;
      if (x.status === 'completed') continue;
      const spent = x.status === 'in_progress' && x.startedAt ? now - x.startedAt : 0;
      left += Math.max(0, x.est - spent);
      if (spent > x.est) over = { step: x, spent };
    }
    const allDone = this.plan.every((x) => x.status === 'completed');
    const end = allDone ? Math.max(...this.plan.map((x) => x.doneAt || 0)) : now;
    return { total, left, over, spent: Math.max(0, end - this.planBornAt), missing: this.plan.length - withEst.length, allDone };
  }

  // номер задачи TaskCreate приходит в ответе инструмента: «Task #3 created»
  onTaskCreated(d) {
    const task = d.toolUseResult && d.toolUseResult.task;
    const content = d.message && Array.isArray(d.message.content) ? d.message.content : [];
    const res = content.find((b) => b && b.type === 'tool_result');
    const s = res && this.pendingTasks.get(res.tool_use_id);
    if (!task || !s) return;
    s.id = String(task.id);
    this.pendingTasks.delete(res.tool_use_id);
  }

  pullSubagents() {
    const dir = path.join(path.dirname(this.main.file), this.id, 'subagents');
    let names;
    try { names = fs.readdirSync(dir); } catch { return; }
    const list = [];
    for (const n of names) {
      if (!n.endsWith('.jsonl')) continue;
      const file = path.join(dir, n);
      let t = this.subs.get(file);
      if (!t) this.subs.set(file, (t = new Transcript(file)));
      const sub = t;
      sub.pull((d) => { for (const b of toolUses(d)) sub.last = action(b, d.timestamp); });
      const key = n.slice(0, -6);
      let meta = this.metas.get(key);
      if (!meta || !meta.finished) {
        try {
          meta = JSON.parse(fs.readFileSync(path.join(dir, key + '.meta.json'), 'utf8'));
          this.metas.set(key, meta);
        } catch {}
      }
      list.push({
        name: (meta && (meta.description || meta.agentType)) || key,
        type: (meta && meta.agentType) || '',
        model: t.model || (meta && meta.model) || '?',
        // finished: true появляется в meta только по завершении; без него агент считаем
        // работающим, пока его транскрипт менялся в последние полчаса
        running: !(meta && meta.finished) && Date.now() - t.mtime < 30 * 60e3,
        t,
      });
    }
    this.agentList = list;
  }

  get running() {
    return this.agentList.filter((a) => a.running);
  }

  // самое свежее действие: главного агента или одного из работающих субагентов
  get activity() {
    let best = this.last && { ...this.last, who: '' };
    for (const a of this.running) {
      if (a.t.last && (!best || a.t.last.at > best.at)) best = { ...a.t.last, who: a.name };
    }
    return best;
  }

  get tokens() {
    const t = this.main ? this.main.totals : { i: 0, wi: 0, c: 0, o: 0 };
    for (const a of this.agentList) { const s = a.t.totals; t.i += s.i; t.wi += s.wi; t.c += s.c; t.o += s.o; }
    return t;
  }

  // токены по моделям: строки главного агента и строки субагентов с их числом и временем
  rows(now) {
    const rows = [];
    if (this.main) {
      for (const [m, b] of this.main.byModel) rows.push({ label: 'главный · ' + shortModel(m), ...b, agents: null, ms: null });
    }
    const subs = new Map();
    const row = (m) => {
      let r = subs.get(m);
      if (!r) subs.set(m, (r = { label: 'агенты · ' + shortModel(m), i: 0, wi: 0, c: 0, o: 0, agents: 0, ms: 0 }));
      return r;
    };
    for (const a of this.agentList) {
      for (const [m, b] of a.t.byModel) { const r = row(m); r.i += b.i; r.wi += b.wi; r.c += b.c; r.o += b.o; }
      const r = row(a.t.model || a.model);
      r.agents++;
      r.ms += agentTime(a, now);
    }
    return rows.concat([...subs.values()].sort((a, b) => b.o - a.o));
  }

  // Советы по экономии токенов. Считаются локально по транскриптам, цена каждого — порядок величины.
  // Пересчёт не чаще раза в 2 секунды: у больших сессий десятки тысяч вызовов.
  tips(now) {
    if (this.tipsCache && now - this.tipsCache.at < 2000) return this.tipsCache.list;
    const list = [];
    const main = this.main;
    if (main) {
      const subs = this.agentList.map((a) => a.t);
      const lead = (key) => `Совет от монитора ccwatch (ccwatch:${key}) по экономии токенов, пользователь просит его учесть: `;
      const tail = '{{ОТВЕТ}}'; // ниже заменяется просьбой ответить строкой с ключом этого совета

      // 1. Большие выводы в контексте главного агента: каждый перечитывается из кэша на каждом запросе
      //    до следующего сжатия контекста.
      const reqT = main.reqs.map((r) => r.t);
      const rereads = (t) => {
        const c = main.compactions.find((x) => x > t);
        return Math.max(0, (c ? countBefore(reqT, c) : reqT.length) - countBefore(reqT, t + 1));
      };
      const heavy = new Map();
      for (const c of main.calls) {
        const size = main.results.get(c.id);
        if (!size || size < 1500) continue;
        const rr = rereads(c.t);
        const key = c.key || c.label;
        const h = heavy.get(key) || { label: c.label, size: 0, rr: 0, cost: 0 };
        h.size = Math.max(h.size, size);
        h.rr = Math.max(h.rr, rr);
        h.cost += size * rr;
        heavy.set(key, h);
      }
      const top = [...heavy.values()].filter((h) => h.cost >= 200e3).sort((a, b) => b.cost - a.cost).slice(0, 4);
      const heavyCost = top.reduce((s, h) => s + h.cost, 0);
      if (heavyCost >= 500e3) {
        const items = top.map((h) => `${clip(h.label, 50)} (~${tok(h.size)}, перечитан ${times(h.rr)})`);
        list.push({
          key: 'heavy', title: 'Большие выводы держатся в контексте', cost: heavyCost, unit: 'из кэша', send: true,
          detail: items.join('; '),
          text: lead('heavy') + `в контексте этой сессии лежат большие выводы, они перечитываются из кэша на каждом запросе: ${items.join('; ')}. `
            + 'Дальше читай файлы кусками (Read с offset/limit или grep -n по нужному разделу), длинные выводы команд обрезай '
            + '(| tail -n 30, тихие флаги), а большие файлы разбирай через субагента и бери от него короткую выжимку.' + tail,
        });
      }

      // 2. Одни и те же файлы читают многие субагенты
      const files = new Map();
      subs.forEach((t, ai) => {
        for (const c of t.calls) {
          if (!c.key.startsWith('read:')) continue;
          const f = files.get(c.key) || { label: c.label, n: 0, agents: new Set(), cost: 0 };
          f.n++;
          f.agents.add(ai);
          f.cost += t.results.get(c.id) || 0;
          files.set(c.key, f);
        }
      });
      const shared = [...files.values()].filter((f) => f.agents.size >= 5).sort((a, b) => b.cost - a.cost).slice(0, 4);
      const sharedCost = shared.reduce((s, f) => s + f.cost, 0);
      if (sharedCost >= 200e3) {
        const items = shared.map((f) => `${clip(f.label, 40)} (${f.agents.size} ${plural(f.agents.size, 'агент', 'агента', 'агентов')}, ${times(f.n)}, ~${tok(f.cost)})`);
        list.push({
          key: 'shared', title: 'Субагенты читают одни и те же файлы', cost: sharedCost, unit: 'на входе', send: true,
          detail: items.join('; '),
          text: lead('shared') + `субагенты этой сессии снова и снова читают одни и те же файлы: ${items.join('; ')}. `
            + 'Когда запускаешь субагентов, передавай им в задании нужный фрагмент или путь к короткой выжимке '
            + '(отдельный файл только с их разделом), чтобы каждый не читал общий файл целиком.' + tail,
        });
      }

      // 3. Повторяющиеся команды: каждый вызов — ещё один запрос, который заново читает весь контекст агента.
      //    Повтор — когда один агент сам запускает одно и то же 3+ раз; «каждый агент один раз закоммитил» — норма.
      const perAgent = new Map(); // команда → (агент → [контекст каждого вызова])
      const addCmds = (t, owner) => {
        for (const c of t.calls) {
          if (!c.key.startsWith('cmd:')) continue;
          let by = perAgent.get(c.key);
          if (!by) perAgent.set(c.key, (by = new Map()));
          if (!by.has(owner)) by.set(owner, []);
          by.get(owner).push(c.ctx);
        }
      };
      addCmds(main, -1);
      subs.forEach(addCmds);
      const reps = [];
      for (const [key, by] of perAgent) {
        const r = { label: key.slice(4), n: 0, agents: new Set(), cost: 0 };
        for (const [owner, ctxs] of by) {
          if (ctxs.length < 3) continue;
          r.n += ctxs.length;
          r.agents.add(owner);
          r.cost += ctxs.reduce((s, x) => s + x, 0);
        }
        if (r.n >= 8) reps.push(r);
      }
      reps.sort((a, b) => b.cost - a.cost).splice(3);
      const repCost = reps.reduce((s, r) => s + r.cost, 0);
      if (repCost >= 500e3) {
        const who = (r) => (r.agents.size > 1 || !r.agents.has(-1) ? ` у ${r.agents.size} ${plural(r.agents.size, 'агента', 'агентов', 'агентов')}` : '');
        const items = reps.map((r) => `«${clip(r.label, 60)}» — ${times(r.n)}${who(r)}`);
        list.push({
          key: 'repeat', title: 'Одни и те же команды запускаются много раз', cost: repCost, unit: 'из кэша', send: true,
          detail: items.join('; '),
          text: lead('repeat') + `одни и те же команды запускаются много раз: ${items.join('; ')}. Каждый запуск — отдельный запрос модели, `
            + 'который заново читает весь контекст. Сверни повторяющуюся последовательность в скрипт (например, в tools/), '
            + 'который делает всё за один вызов и печатает только итог, и давай субагентам готовую команду.' + tail,
        });
      }

      // 4. Разросшиеся субагенты: агент с контекстом за 200k дочитывает хвост работы в разы дороже свежего
      //    (замер из «Доклада по токенам»: 655k-агент — 135M за час, свежий доделал тот же хвост за 5,6M)
      const bloated = this.agentList.filter((a) => a.t.maxCtx >= 200e3);
      const bigReads = bloated.reduce((s, a) => s + a.t.bigReads, 0);
      if (bloated.length && bigReads >= 2e6) {
        const items = bloated.sort((a, b) => b.t.maxCtx - a.t.maxCtx).slice(0, 3).map((a) => `${clip(a.name, 40)} (до ${tok(a.t.maxCtx)})`);
        list.push({
          key: 'bloated', title: 'Субагенты разрастаются', cost: bigReads, unit: 'из кэша при контексте >200k', send: true, w: bigReads * PRICE.read,
          detail: `${bloated.length} ${plural(bloated.length, 'агент работал', 'агента работали', 'агентов работали')} с контекстом больше 200k: ${items.join('; ')}.`,
          text: lead('bloated') + `субагенты этой сессии разрастаются: ${items.join('; ')}. Агент с большим контекстом перечитывает его на каждом шаге, `
            + 'поэтому хвост работы у него стоит в разы дороже, чем у свежего. Давай субагентам задачи поменьше, не продолжай '
            + 'через SendMessage агента с контекстом за 150–200k, а запускай нового с короткой выжимкой того, что сделано.' + tail,
        });
      }

      // 5. Механика у главного агента: если он в основном гоняет команды, это дешевле делать субагенту на Sonnet
      const shell = main.calls.filter((c) => c.shell);
      if (shell.length >= 100 && shell.length >= 0.5 * main.calls.length && /opus/.test(main.model)) {
        const cost = shell.reduce((s, c) => s + c.ctx, 0);
        list.push({
          key: 'shell', title: 'Главный агент сам делает механику', cost, unit: 'из кэша на командах', send: true, w: cost * PRICE.read,
          detail: `${shell.length} из ${main.calls.length} вызовов главного агента — команды (сборки, прогоны, слияния). `
            + `Каждая перечитывает его контекст на ${shortModel(main.model)}.`,
          text: lead('shell') + `${shell.length} из ${main.calls.length} твоих вызовов — команды оболочки. Механическую работу (сборки, прогоны тестов, `
            + 'слияния, проверки) отдавай субагенту на Sonnet пачкой с понятным критерием готовности, а сам оставляй решения.' + tail,
        });
      }

      // 6. Перезапись кэша после пауз — агент тут не поможет, это для пользователя
      const all = [main].concat(subs);
      const rewrites = all.reduce((s, t) => s + t.rewrites, 0);
      const rewriteTok = all.reduce((s, t) => s + t.rewriteTok, 0);
      const subs5m = subs.reduce((s, t) => s + t.rewrite5m, 0);
      if (rewriteTok >= 300e3) {
        const short = subs5m > 0.3 * rewriteTok; // больше всего переписывают субагенты с 5-минутным кэшем
        const tip = {
          key: 'cache', title: 'Кэш переписывался заново', cost: rewriteTok, unit: 'записи', send: false,
          w: all.reduce((s, t) => s + t.rewriteW, 0),
          detail: `${times(rewrites)} контекст записывался в кэш почти целиком: после паузы дольше срока жизни кэша или после `
            + 'сжатия контекста. Токенов немного, но запись в 12–20 раз дороже чтения.'
            + (short ? ' Больше всего переписывают субагенты с 5-минутным кэшем — Enter: скриптом продлить их кэш до часа.' : '')
            + ' Перед долгой паузой дешевле сжать контекст (/compact) — это вручную.',
        };
        if (short) {
          tip.fix = {
            what: 'кэш субагентов на час', changes: [{ path: ['subagentPromptCacheTtl'], to: '1h' }],
            note: 'для всех сессий: запись в кэш на час дороже (×20 к чтению вместо ×12,5), зато кэш субагента переживает паузы до часа; '
              + 'при перерасходе подписки час не действует',
          };
        }
        list.push(tip);
      }

      // 7. Очень большой контекст главного агента — тоже решение пользователя
      if (this.ctx >= 300e3) {
        const m = main.model;
        const tip = {
          key: 'ctx', title: `Контекст главного агента ${tok(this.ctx)}`, cost: this.ctx, unit: 'на каждый запрос', send: false,
          w: this.ctx * PRICE.read,
          detail: 'Каждый запрос перечитывает его целиком. Если начинаете новую большую задачу, помогут /compact или новая сессия — это вручную.',
        };
        if (/^claude-/.test(m)) {
          // окно автосжатия по модели — та же настройка, что сохраняет /autocompact
          tip.detail += ` Enter — скриптом включить автосжатие раньше: на ${tok(COMPACT_AT)} вместо почти 1M.`;
          tip.fix = {
            what: `автосжатие ${shortModel(m)} на ${tok(COMPACT_AT)}`, changes: [{ path: ['modelSettings', m, 'autoCompactWindow'], to: COMPACT_AT }],
            note: `для всех сессий на ${shortModel(m)}: разговор сжимается, дойдя до ~${tok(COMPACT_AT)} токенов; при сжатии старые подробности заменяются выжимкой`,
          };
        }
        list.push(tip);
      }
    }
    // ранжируем по цене: чтение кэша ×0,1, вход в контекст ×1,25 (запись), записи — по их сроку жизни
    for (const t of list) if (t.w == null) t.w = t.cost * (t.unit === 'на входе' ? PRICE.write5m : PRICE.read);
    list.sort((a, b) => b.w - a.w);
    const all = list.concat(this.speedTips());
    // агент отвечает строкой в обычном тексте — ccwatch прочитает её из транскрипта
    for (const t of all) if (t.text) t.text = t.text.replace('{{ОТВЕТ}}', ackRequest(t.key));
    this.tipsCache = { at: now, list: all };
    return all;
  }

  // Советы по скорости: где агенты теряют время. Цена — минуты работы агентов (у параллельных складываются).
  speedTips() {
    const main = this.main;
    if (!main) return [];
    const owners = [main].concat(this.agentList.map((a) => a.t));
    const lead = (key) => `Совет от монитора ccwatch (ccwatch:${key}) по скорости работы, пользователь просит его учесть: `;
    const slow = new Map();
    const small = { n: 0, ms: 0 };
    const poll = { n: 0, ms: 0 };
    let think = 0;
    let model = 0;
    let reqs = 0;
    let multi = 0;
    const thinkBy = new Map(); // «модель effort» → мс размышлений у всех агентов
    const limit = { n: 0, ms: 0, poll: 0, max: 0 }; // упор в тайм-аут; max — в предел 10 минут
    for (const t of owners) {
      think += t.thinkMs;
      for (const [k, ms] of t.thinkBy) thinkBy.set(k, (thinkBy.get(k) || 0) + ms);
      model += t.modelMs();
      for (const r of t.reqs) if (r.tools) { reqs++; if (r.tools > 1) multi++; }
      for (const c of t.calls) {
        const at = t.resultAt.get(c.id);
        if (!c.shell || !at) continue;
        const dt = Math.max(0, at - c.t);
        if (hitLimit(c, dt)) { limit.n++; limit.ms += dt; if (c.poll) limit.poll++; if (dt >= 590e3) limit.max++; }
        if (c.poll) { poll.n++; poll.ms += dt; continue; }
        if (TRIVIAL.test(c.prog)) { small.n++; small.ms += dt; continue; }
        if (!c.key.startsWith('cmd:')) continue;
        const g = slow.get(c.key) || { label: c.key.slice(4), n: 0, ms: 0 };
        g.n++;
        g.ms += dt;
        slow.set(c.key, g);
      }
    }
    const out = [];
    const tail = '{{ОТВЕТ}}';

    const top = [...slow.values()].filter((g) => g.n >= 3 && g.ms / g.n >= 60e3).sort((a, b) => b.ms - a.ms).slice(0, 3);
    const slowMs = top.reduce((s, g) => s + g.ms, 0);
    if (slowMs >= 10 * 60e3) {
      const items = top.map((g) => `«${clip(g.label, 50)}» — ${g.n} ${plural(g.n, 'запуск', 'запуска', 'запусков')}, в среднем ${dur(g.ms / g.n)}, всего ${dur(g.ms)}`);
      out.push({
        key: 'slowcheck', kind: 'time', title: 'Долгие проверки', cost: slowMs, send: true, detail: items.join('; '),
        text: lead('slowcheck') + `долгие проверки отнимают много времени: ${items.join('; ')}. По ходу работы гоняй точечные проверки, `
          + 'полный прогон — один раз в конце; не повторяй прогон, который уже прошёл, если код не менялся; независимые наборы '
          + 'и сиды запускай параллельно (в фоне или пачкой в одном скрипте).' + tail,
      });
    }
    if (small.n >= 50 && small.ms >= 10 * 60e3) {
      const avg = (small.ms / small.n / 1000).toFixed(1);
      out.push({
        key: 'smallshell', kind: 'time', title: 'Мелкие команды оболочки', cost: small.ms, send: true,
        detail: `${small.n} вызовов cat, grep, sed, ls и подобных, в среднем ${avg} с каждый; несколько инструментов сразу вызывают только ${pct(multi / Math.max(1, reqs))} запросов.`,
        text: lead('smallshell') + `мелкие команды оболочки (cat, grep, sed, ls и подобные) — ${small.n} вызовов, в среднем ${avg} с каждый. `
          + 'Читай и ищи встроенными Read, Grep и Glob, а независимые чтения и поиски запрашивай одним сообщением сразу.' + tail,
      });
    }
    if (poll.ms >= 5 * 60e3) {
      out.push({
        key: 'polling', kind: 'time', title: 'Ожидание в цикле', cost: poll.ms, send: true,
        detail: `${poll.n} ${plural(poll.n, 'команда', 'команды', 'команд')} с циклом until/while/for и sleep, всего ${dur(poll.ms)}.`,
        text: lead('polling') + `агенты ждут в цикле (until/while/for со sleep): ${times(poll.n)}, всего ${dur(poll.ms)}. Долгие команды запускай в фоне `
          + 'и занимайся другим, результат придёт уведомлением; не опрашивай файл в цикле.' + tail,
      });
    }

    // Повторная полная проверка: тот же набор на коде, который уже проверяли (тот же коммит или правок не было)
    const runs = this.checkRuns();
    const again = runs.filter((r) => r.repeat);
    const againMs = again.reduce((s, r) => s + r.dt, 0);
    if (again.length >= 5 && againMs >= 10 * 60e3) {
      const bySuite = new Map();
      for (const r of again) { const g = bySuite.get(r.c.suite) || { n: 0, ms: 0 }; g.n++; g.ms += r.dt; bySuite.set(r.c.suite, g); }
      const items = [...bySuite].sort((a, b) => b[1].ms - a[1].ms).slice(0, 3).map(([k, g]) => `«${clip(k, 50)}» — ${times(g.n)}, ${dur(g.ms)}`);
      const other = again.filter((r) => r.byOther).length;
      const roles = this.checkShare(Date.now());
      const parts = [`${again.length} из ${runs.length} ${plural(runs.length, 'прогона', 'прогонов', 'прогонов')} проверок повторили набор на уже проверенном коде, всего ${dur(againMs)}`
        + (other ? `; ${other} — на коммите, который уже проверил другой агент` : '') + '.', `Чаще всего: ${items.join('; ')}.`];
      if (roles) parts.push(`На проверяющих агентов (тесты, проверки, ревью — по типу агента или заданию) ушло ${pct(roles)} времени субагентов.`);
      out.push({
        key: 'recheck', kind: 'time', title: 'Повторные проверки на том же коде', cost: againMs, send: true, detail: parts.join(' '),
        text: lead('recheck') + `одни и те же наборы проверок прогоняются повторно на коде, который уже проверен: ${again.length} ${plural(again.length, 'прогон', 'прогона', 'прогонов')}, `
          + `всего ${dur(againMs)} (${items.join('; ')}). Перед полным прогоном смотри, не проверен ли уже этот коммит (лог или отметка с хешем); `
          + 'не запускай прошедший набор снова, если код не менялся; не поручай один и тот же полный набор и исполнителю, и проверяющему — '
          + 'проверяющий берёт готовый лог исполнителя и сам гоняет только то, чего там нет.' + tail,
      });
    }

    // Упор в тайм-аут: команда шла до лимита инструмента — чаще всего это ожидание в цикле или зависший прогон
    if (limit.n >= 3 && limit.ms >= 15 * 60e3) {
      const loops = limit.poll ? ` Циклов ожидания среди них — ${limit.poll}.` : '';
      const max = limit.max ? `: ${limit.max} — до предела в 10 минут, остальные — до 2 минут по умолчанию или заданного тайм-аута` : '';
      out.push({
        key: 'timeout', kind: 'time', title: 'Команды упираются в тайм-аут', cost: limit.ms, send: true,
        detail: `${limit.n} ${plural(limit.n, 'команда шла', 'команды шли', 'команд шли')} до своего тайм-аута, всего ${dur(limit.ms)}${max}.${loops} `
          + 'Всё это время агент стоит и ждёт.',
        text: lead('timeout') + `команды упираются в тайм-аут инструмента: ${limit.n} ${plural(limit.n, 'вызов', 'вызова', 'вызовов')}, всего ${dur(limit.ms)}${loops}. `
          + 'Долгие прогоны и ожидание файла или процесса запускай в фоне (run_in_background) и жди уведомления, а не держи вызов до лимита; '
          + 'если ждёшь в команде — ставь короткий явный тайм-аут (timeout 120 …) и печатай понятный итог при неудаче.' + tail,
      });
    }

    // Долгий разгон: от задания субагенту до его первой правки кода (Write/Edit), без перерывов
    const ramps = this.agentList.filter((a) => a.t.rampMs >= 0);
    const rampMs = ramps.reduce((s, a) => s + Math.max(0, a.t.rampMs - RAMP_OK), 0);
    if (ramps.length >= 3 && rampMs >= 20 * 60e3) {
      const byModel = new Map();
      for (const a of ramps) {
        const g = byModel.get(shortModel(a.t.model || a.model)) || { n: 0, ms: 0 };
        g.n++;
        g.ms += a.t.rampMs;
        byModel.set(shortModel(a.t.model || a.model), g);
      }
      const items = [...byModel].sort((a, b) => b[1].ms / b[1].n - a[1].ms / a[1].n)
        .map(([m, g]) => `у ${m} в среднем ${approx(g.ms / g.n)} (${g.n} ${plural(g.n, 'агент', 'агента', 'агентов')})`);
      const now = Date.now();
      const all = ramps.reduce((s, a) => s + a.t.rampMs, 0);
      const work = ramps.reduce((s, a) => s + agentTime(a, now), 0);
      const share = work ? ` Это ${pct(all / work)} времени агентов, которые правили код.` : '';
      out.push({
        key: 'ramp', kind: 'time', title: 'Субагенты долго разгоняются', cost: rampMs, send: true,
        detail: `До первой правки кода: ${items.join('; ')}.${share} Цена — время сверх первых ${dur(RAMP_OK)} на каждого.`,
        text: lead('ramp') + `субагенты долго разгоняются: до первой правки кода ${items.join('; ')}.${share} `
          + 'В задании исполнителю сразу давай файлы и места правки, нужные выжимки и критерий готовности, чтобы он не изучал проект заново; '
          + 'если нужно исследование — сделай его одним коротким агентом и передай исполнителям итог.' + tail,
      });
    }
    if (think >= 20 * 60e3 && model && think >= 0.4 * model) {
      // больше всех размышляет одна пара «модель + effort» — её и предлагаем снизить на ступень
      const [topKey, topMs] = [...thinkBy].sort((a, b) => b[1] - a[1])[0] || ['', 0];
      const [tm, te] = topKey.split(' ');
      const target = { max: 'high', xhigh: 'high', high: 'medium' }[te];
      const tip = {
        key: 'thinking', kind: 'time', title: `Размышления — ${pct(think / model)} времени модели`, cost: think, send: !!target,
        detail: `${dur(think)} из ${dur(model)} работы модели ушло на размышления, из них ${dur(topMs)} — ${shortModel(tm)} на effort ${te}.`,
      };
      if (target) {
        // два способа: попросить эту сессию (effort в вызове Agent) или поменять настройку для всех сессий
        tip.detail += ` Исполнителям обычно хватает effort ${target}. Enter — попросить эту сессию запускать исполнителей с effort ${target}`;
        tip.text = lead('thinking') + `размышления заняли ${pct(think / model)} времени модели (${dur(think)} из ${dur(model)}), `
          + `больше всего у ${shortModel(tm)} на effort ${te}. Пользователь просит запускать субагентов-исполнителей (правка кода, проверки, документы) `
          + `с effort ${target}: передавай effort: "${target}" в вызове Agent. Effort ${te} оставляй для задач, где нужен глубокий анализ: `
          + 'архитектура, сложная отладка.' + tail;
        if (/^claude-/.test(tm)) {
          tip.fix = {
            what: `effort ${target} для ${shortModel(tm)}`, note: `для всех сессий и субагентов на ${shortModel(tm)}, в том числе ваших`,
            changes: [{ path: ['modelSettings', tm, 'effortLevel'], to: target }],
          };
          tip.detail += `; u — скриптом поставить ${target} для ${shortModel(tm)} в ~/.claude/settings.json (для всех сессий).`;
        } else tip.detail += '.';
      }
      out.push(tip);
    }
    return out.sort((a, b) => b.cost - a.cost);
  }

  // Прогоны проверок всех агентов по времени. Повтор — тот же набор на том же коде: в выводе тот же коммит
  // у каждого набора, а если коммита в выводе нет — тот же агент не менял код с прошлого такого прогона.
  checkRuns() {
    const runs = [];
    [this.main].concat(this.agentList.map((a) => a.t)).forEach((t, owner) => {
      for (const c of t.calls) {
        const at = c.check && t.resultAt.get(c.id);
        if (at) runs.push({ owner, t: c.t, dt: Math.max(0, at - c.t), c, repeat: false, byOther: false });
      }
    });
    runs.sort((a, b) => a.t - b.t);
    const checked = new Map(); // набор@коммит → кто проверил первым
    const lastVer = new Map(); // агент и набор → версия его кода при прошлом прогоне
    for (const r of runs) {
      const keys = r.c.hashes || [];
      const lk = r.owner + ' ' + r.c.suite;
      if (keys.length) {
        const by = keys.map((k) => checked.get(k));
        r.repeat = by.every((x) => x != null);
        r.byOther = r.repeat && by.some((x) => x !== r.owner);
        for (const k of keys) if (!checked.has(k)) checked.set(k, r.owner);
      } else r.repeat = lastVer.get(lk) === r.c.ver;
      lastVer.set(lk, r.c.ver);
    }
    return runs;
  }

  // доля времени субагентов-проверяющих: тип агента или его задание про тесты, проверки, ревью
  checkShare(now) {
    let all = 0;
    let mine = 0;
    for (const a of this.agentList) {
      const ms = agentTime(a, now);
      all += ms;
      if (CHECK_ROLE.test(a.type + ' ' + a.name)) mine += ms;
    }
    return all && mine ? mine / all : 0;
  }

  // Эффект доставленных советов: своя мера у каждого совета до доставки (за 3 часа) и после неё.
  // Это корреляция, а не доказательство: мера может сдвинуться и от смены фазы задачи.
  effects(now) {
    if (this.effCache && now - this.effCache.at < 5000) return this.effCache.list;
    const out = [];
    const main = this.main;
    if (main && main.deliveries.length) {
      const latest = new Map(); // по каждому совету берём последнюю доставку
      for (const d of main.deliveries) if (!isMsg(d.key) && (!latest.has(d.key) || d.t > latest.get(d.key))) latest.set(d.key, d.t);
      const agents = this.agentList;
      const owners = [{ id: -1, t: main }].concat(agents.map((a, i) => ({ id: i, t: a.t })));
      // файлы, которые за сессию читали 5+ субагентов
      const readers = new Map();
      agents.forEach((a, i) => {
        for (const c of a.t.calls) {
          if (!c.key.startsWith('read:')) continue;
          if (!readers.has(c.key)) readers.set(c.key, new Set());
          readers.get(c.key).add(i);
        }
      });
      const shared = new Set([...readers].filter(([, s]) => s.size >= 5).map(([k]) => k));

      for (const [key, t] of latest) {
        const from = t - 3 * 3600e3;
        const inB = (x) => x >= from && x < t;
        const inA = (x) => x >= t;
        let m = null;
        if (key === 'bloated' || key === 'shared') {
          // по субагентам, запущенным до и после совета
          const pick = (inW) => agents.filter((a) => a.t.first && inW(a.t.first));
          const per = key === 'bloated'
            ? (a) => a.t.maxCtx
            : (a) => a.t.calls.filter((c) => shared.has(c.key)).reduce((s, c) => s + (a.t.results.get(c.id) || 0), 0);
          const avg = (list) => (list.length ? list.reduce((s, a) => s + per(a), 0) / list.length : 0);
          const B = pick(inB);
          const A = pick(inA);
          m = {
            what: key === 'bloated' ? 'макс. контекст нового субагента' : 'чтение общих файлов на субагента',
            before: avg(B), after: avg(A), fmt: tok,
            n: `${A.length} ${plural(A.length, 'новый агент', 'новых агента', 'новых агентов')}`,
            enough: A.length >= 2 && B.length >= 1,
          };
        } else if (key === 'repeat') {
          // доля повторных запусков: каждый запуск той же программы тем же агентом сверх первого
          const share = (inW) => {
            let total = 0;
            let rep = 0;
            for (const o of owners) {
              const by = new Map();
              for (const c of o.t.calls) if (c.key.startsWith('cmd:') && inW(c.t)) by.set(c.key, (by.get(c.key) || 0) + 1);
              for (const n of by.values()) { total += n; rep += n - 1; }
            }
            return { v: total ? rep / total : 0, total };
          };
          const B = share(inB);
          const A = share(inA);
          m = {
            what: 'доля повторных запусков', before: B.v, after: A.v, fmt: pct,
            n: `${A.total} ${plural(A.total, 'запуск', 'запуска', 'запусков')}`, enough: A.total >= 20 && B.total >= 10,
          };
        } else if (key === 'heavy' || key === 'shell') {
          // по вызовам главного агента
          const calls = (inW) => main.calls.filter((c) => inW(c.t));
          const B = calls(inB);
          const A = calls(inA);
          const val = key === 'heavy'
            ? (list) => (list.length ? list.reduce((s, c) => s + (main.results.get(c.id) || 0), 0) / list.length : 0)
            : (list) => (list.length ? list.filter((c) => c.shell).length / list.length : 0);
          m = {
            what: key === 'heavy' ? 'средний вывод инструмента у главного агента' : 'доля команд у главного агента',
            before: val(B), after: val(A), fmt: key === 'heavy' ? tok : pct,
            n: `${A.length} ${plural(A.length, 'вызов', 'вызова', 'вызовов')}`, enough: A.length >= 20 && B.length >= 10,
          };
        }
        if (key === 'slowcheck' || key === 'smallshell' || key === 'polling') {
          // скорость: на один вызов инструмента — доля мелких команд или секунды долгих проверок / ожидания в цикле
          const per = (inW) => {
            let n = 0;
            let v = 0;
            for (const o of owners) {
              for (const c of o.t.calls) {
                if (!inW(c.t)) continue;
                n++;
                if (!c.shell) continue;
                const at = o.t.resultAt.get(c.id);
                const dt = at ? Math.max(0, at - c.t) : 0;
                if (key === 'smallshell') { if (!c.poll && TRIVIAL.test(c.prog)) v++; }
                else if (key === 'polling') { if (c.poll) v += dt; }
                else if (!c.poll && !TRIVIAL.test(c.prog) && dt >= 60e3) v += dt;
              }
            }
            return { v: n ? v / n : 0, n };
          };
          const B = per(inB);
          const A = per(inA);
          m = {
            what: key === 'smallshell' ? 'доля мелких команд оболочки' : key === 'polling' ? 'ожидание в цикле на вызов' : 'долгие проверки на вызов',
            before: B.v, after: A.v, fmt: key === 'smallshell' ? pct : (x) => (x / 1000).toFixed(1) + ' с',
            n: `${A.n} ${plural(A.n, 'вызов', 'вызова', 'вызовов')}`, enough: A.n >= 30 && B.n >= 30,
          };
        }
        if (key === 'recheck') {
          // доля повторов среди прогонов проверок
          const runs = this.checkRuns();
          const share = (inW) => {
            const list = runs.filter((r) => inW(r.t));
            return { v: list.length ? list.filter((r) => r.repeat).length / list.length : 0, n: list.length };
          };
          const B = share(inB);
          const A = share(inA);
          m = {
            what: 'доля повторных прогонов проверок', before: B.v, after: A.v, fmt: pct,
            n: `${A.n} ${plural(A.n, 'прогон', 'прогона', 'прогонов')}`, enough: A.n >= 10 && B.n >= 10,
          };
        } else if (key === 'timeout') {
          // время команд, упёршихся в лимит, на одну команду
          const per = (inW) => {
            let n = 0;
            let v = 0;
            for (const o of owners) {
              for (const c of o.t.calls) {
                if (!c.shell || !inW(c.t)) continue;
                const at = o.t.resultAt.get(c.id);
                if (!at) continue;
                n++;
                if (hitLimit(c, at - c.t)) v += at - c.t;
              }
            }
            return { v: n ? v / n : 0, n };
          };
          const B = per(inB);
          const A = per(inA);
          m = {
            what: 'упор в тайм-аут на команду', before: B.v, after: A.v, fmt: (x) => (x / 1000).toFixed(1) + ' с',
            n: `${A.n} ${plural(A.n, 'команда', 'команды', 'команд')}`, enough: A.n >= 30 && B.n >= 30,
          };
        } else if (key === 'ramp') {
          // разгон субагентов, запущенных до и после совета
          const pick = (inW) => agents.filter((a) => a.t.rampMs >= 0 && inW(a.t.first));
          const avg = (list) => (list.length ? list.reduce((s, a) => s + a.t.rampMs, 0) / list.length : 0);
          const B = pick(inB);
          const A = pick(inA);
          m = {
            what: 'разгон нового субагента до первой правки', before: avg(B), after: avg(A), fmt: dur,
            n: `${A.length} ${plural(A.length, 'новый агент', 'новых агента', 'новых агентов')}`, enough: A.length >= 2 && B.length >= 1,
          };
        } else if (key === 'thinking') {
          // доля размышлений во времени модели у субагентов, запущенных до и после совета
          const pick = (inW) => agents.filter((a) => inW(a.t.first));
          const share = (list) => {
            const ms = list.reduce((s, a) => s + a.t.modelMs(), 0);
            return ms ? list.reduce((s, a) => s + a.t.thinkMs, 0) / ms : 0;
          };
          const B = pick(inB);
          const A = pick(inA);
          m = {
            what: 'доля размышлений у новых субагентов', before: share(B), after: share(A), fmt: pct,
            n: `${A.length} ${plural(A.length, 'новый агент', 'новых агента', 'новых агентов')}`, enough: A.length >= 2 && B.length >= 2,
          };
        }
        if (!m) continue;
        // общая мера: средний контекст запроса по всем агентам
        const ctxAvg = (inW) => {
          let s = 0;
          let n = 0;
          for (const o of owners) for (const r of o.t.reqs) if (inW(r.t)) { s += r.ctx; n++; }
          return { v: n ? s / n : 0, n };
        };
        const cB = ctxAvg(inB);
        const cA = ctxAvg(inA);
        const since = now - t;
        m.enough = m.enough && since >= 15 * 60e3;
        out.push({
          key, t, since, ...m, verdict: verdict(m.before, m.after, m.enough),
          overall: cA.n >= 30 && cB.n >= 30 ? { before: cB.v, after: cA.v } : null,
        });
      }
      out.sort((a, b) => b.t - a.t);
    }
    this.effCache = { at: now, list: out };
    return out;
  }

  // Скиллы по логам: кто вызывал (главный агент, субагент или вы через «/») и когда.
  skillUse() {
    const events = [];
    if (this.main) for (const e of this.main.skills) events.push({ ...e, who: e.by === 'user' ? 'вы' : 'главный агент' });
    for (const a of this.agentList) for (const e of a.t.skills) events.push({ ...e, who: 'агент «' + clip(a.name, 40) + '»' });
    events.sort((a, b) => b.t - a.t);
    const bySkill = new Map();
    for (const e of events) {
      if (e.by === 'user' && BUILTIN.has(e.name)) continue; // встроенные команды — не скиллы
      const r = bySkill.get(e.name) || { n: 0, who: new Map() };
      r.n++;
      r.who.set(e.who, (r.who.get(e.who) || 0) + 1);
      bySkill.set(e.name, r);
    }
    const agent = events.filter((e) => e.by === 'agent').length;
    const userSkills = events.filter((e) => e.by === 'user' && !BUILTIN.has(e.name)).length;
    const builtins = events.filter((e) => e.by === 'user' && BUILTIN.has(e.name)).length;
    return { events, bySkill, agent, userSkills, builtins };
  }

  // Активные советы: ещё не доставленные и те, что доставлены, но эффекта после них не видно.
  // Доставленные с эффектом (или пока без данных) уходят в «Отправленные» — не мозолят глаза при перезапусках.
  activeTips(now) {
    const delivered = new Map();
    for (const d of (this.main && this.main.deliveries) || []) {
      if (isMsg(d.key)) continue; // ваши сообщения — не советы
      if (!delivered.has(d.key) || d.t > delivered.get(d.key)) delivered.set(d.key, d.t);
    }
    const effects = new Map(this.effects(now).map((e) => [e.key, e]));
    const active = this.tips(now).filter((t) => {
      if (!delivered.has(t.key)) return true;
      const e = effects.get(t.key);
      return !!(e && e.enough && !e.verdict.text.startsWith('помогло'));
    });
    return { active, delivered, effects };
  }

  get name() {
    const r = this.reg || {};
    return r.name || this.title || path.basename(r.cwd || '') || this.id.slice(0, 8);
  }

  get progress() {
    const steps = this.plan;
    const n = steps.length;
    const done = steps.filter((x) => x.status === 'completed').length;
    const cur = steps.find((x) => x.status === 'in_progress');
    return { n, done, cur, idx: cur ? steps.indexOf(cur) + 1 : 0 };
  }

  status(now) {
    if (!this.live) return { icon: C.dim('○'), text: C.dim('завершена'), busy: false };
    const r = this.reg;
    const since = ago(now - (r.statusUpdatedAt || r.updatedAt || now));
    if (r.status === 'busy') {
      return { icon: C.pink(SPIN[Math.floor(now / 100) % SPIN.length]), text: C.pink('работает ' + since), busy: true };
    }
    const p = this.progress;
    if (r.status === 'idle' && p.n && p.done === p.n) return { icon: C.green('✓'), text: C.green('готово ' + since + ' назад'), busy: false };
    if (r.status === 'idle') return { icon: C.amber('●'), text: C.amber('ждёт вас ' + since), busy: false };
    return { icon: C.amber('●'), text: C.amber(String(r.status || '?')), busy: false };
  }
}

// ── запрос плана ────────────────────────────────────────────────────────────

const hook = { installed: false, checkedAt: 0 };

function checkHook(now) {
  if (now - hook.checkedAt < 5000) return hook.installed;
  hook.checkedAt = now;
  try { hook.installed = fs.readFileSync(path.join(ROOT, 'settings.json'), 'utf8').includes('ccwatch-hook'); } catch { hook.installed = false; }
  return hook.installed;
}

// Запрос к сессии: {at, from, plan — просьба о плане, autoPlan — её ccwatch попросил сам,
// keys / texts — выбранные советы, ttl — сколько ждать доставки}. В старых запросах полей нет — это просьба о плане.
const AUTO_TTL = 10 * 60e3; // автоматическая просьба о плане нужна, только пока открыт дашборд
const MANUAL_TTL = 12 * 3600e3; // то, что вы отправили сами, ждёт, пока сессия снова начнёт работать
const isPlanReq = (r) => r.plan !== false && !(Array.isArray(r.texts) && r.texts.length && r.plan === undefined);

function readAsk(id) {
  try {
    const f = path.join(ASK_DIR, id + '.json');
    const r = JSON.parse(fs.readFileSync(f, 'utf8'));
    return { state: 'wait', at: fs.statSync(f).mtimeMs, plan: isPlanReq(r), texts: r.texts || [], keys: r.keys || [], ttl: r.ttl || AUTO_TTL };
  } catch {}
  try {
    const r = JSON.parse(fs.readFileSync(path.join(DONE_DIR, id + '.json'), 'utf8'));
    return { state: 'sent', at: r.deliveredAt || 0, plan: isPlanReq(r), texts: r.texts || [], keys: r.keys || [] };
  } catch {}
  return null;
}

const autoAsks = new Set(); // автоматические просьбы о плане этого запуска: при выходе снимаем

// дописывает к ещё не доставленному запросу сессии просьбу о плане или совет (тот же совет — заменяет)
function queueAsk(s, add) {
  if (!s || !s.live) return 'Сессия не запущена.';
  try {
    fs.mkdirSync(ASK_DIR, { recursive: true });
    const f = path.join(ASK_DIR, s.id + '.json');
    let cur = { plan: false, auto: false, texts: [], keys: [] };
    try {
      const r = JSON.parse(fs.readFileSync(f, 'utf8'));
      cur = { plan: isPlanReq(r), auto: !!r.autoPlan, texts: r.texts || [], keys: r.keys || [] };
    } catch {}
    const plan = cur.plan || !!add.plan;
    const auto = plan && (add.plan ? !!add.auto && (!cur.plan || cur.auto) : cur.auto);
    const texts = cur.texts.slice();
    const keys = cur.keys.slice();
    if (add.text) {
      const i = keys.indexOf(add.key);
      if (i >= 0) texts[i] = add.text;
      else { texts.push(add.text); keys.push(add.key); }
    }
    const ttl = auto && !texts.length ? AUTO_TTL : MANUAL_TTL;
    fs.writeFileSync(f, JSON.stringify({ at: Date.now(), from: 'ccwatch', plan, autoPlan: auto, keys, texts, ttl }));
    try { fs.unlinkSync(path.join(DONE_DIR, s.id + '.json')); } catch {}
    if (auto) autoAsks.add(s.id);
    s.ask = readAsk(s.id);
  } catch (e) {
    return 'Не удалось записать запрос: ' + e.message;
  }
  return checkHook(0) ? '' : ' Но хук ccwatch-hook.js не подключён: до сессии запрос не дойдёт.';
}

function askPlan(s, auto) {
  const err = queueAsk(s, { plan: true, auto });
  return err.startsWith(' ') || !err ? 'План запрошен.' + err : err;
}

function sendTip(s, tip) {
  if (!tip.send) return 'Этот совет для вас: агент тут ничего изменить не может.';
  const err = queueAsk(s, { key: tip.key, text: tip.text });
  if (err && !err.startsWith(' ')) return err;
  const idle = s.reg && s.reg.status !== 'busy' ? ' Сессия сейчас ждёт вас: совет дойдёт, когда она снова начнёт работать (ждёт до 12 часов).' : '';
  return 'Совет отправлен, дойдёт при следующем действии агента.' + idle + err;
}

// все советы для агента одним сообщением (советы «для вас» агенту не уходят)
function sendAllTips(s, tips) {
  const ok = tips.filter((t) => t.send);
  if (!ok.length) return 'Советов для агента нет: оставшиеся — для вас.';
  for (const t of ok) {
    const msg = sendTip(s, t);
    if (!msg.startsWith('Совет отправлен')) return msg;
  }
  const hookNote = checkHook(0) ? '' : ' Но хук ccwatch-hook.js не подключён: до сессии они не дойдут.';
  return `Отправлено советов: ${ok.length}, дойдут одним сообщением при следующем действии агента.` + hookNote;
}

// Ваше сообщение сессии (клавиша m): тот же хук, метка (ccwatch:msg…) показывает, когда оно дошло.
const sentMsgs = new Map(); // id сессии → [{key, text, at}] за этот запуск

function sendMessage(s, text) {
  const key = 'msg' + Date.now().toString(36);
  const err = queueAsk(s, {
    key,
    text: `Пользователь написал тебе из монитора ccwatch (ccwatch:${key}): «${text}». Отнесись к этому как к его сообщению в чате.`,
  });
  if (err && !err.startsWith(' ')) return err;
  if (!sentMsgs.has(s.id)) sentMsgs.set(s.id, []);
  sentMsgs.get(s.id).push({ key, text, at: Date.now() });
  const idle = s.reg && s.reg.status !== 'busy' ? ' Сессия сейчас ждёт вас: сообщение дойдёт, когда она снова начнёт работать.' : '';
  return 'Сообщение отправлено, дойдёт при следующем действии агента.' + idle + err;
}

// последнее ваше сообщение этой сессии и дошло ли оно
function msgLine(s, now) {
  const list = sentMsgs.get(s.id);
  if (!list || !list.length) return null;
  const m = list[list.length - 1];
  const got = ((s.main && s.main.deliveries) || []).find((d) => d.key === m.key);
  const text = `Ваше сообщение «${clip(m.text, 50)}» `;
  return got ? C.green(text + `дошло ${ago(now - got.t)} назад.`) : C.amber(text + `ждёт доставки (${ago(now - m.at)}).`);
}

// ── скрипты советов «для вас» ───────────────────────────────────────────────
// Совет, который агент выполнить не может, несёт скрипт: tip.fix = {what — что делаем, note — на что повлияет,
// changes — [{path — путь в ~/.claude/settings.json, to — значение}]}. Enter (или u) показывает шаги скрипта,
// y — выполняет. Прежний файл остаётся в settings.json.ccwatch-bak, а что меняли — в ~/.ccwatch/settings-log.json:
// повторный запуск на том же совете возвращает как было.
const SETTINGS = path.join(ROOT, 'settings.json');
const SET_LOG = path.join(BASE, 'settings-log.json');

const getPath = (o, p) => p.reduce((x, k) => (x && typeof x === 'object' ? x[k] : undefined), o);
const readSetLog = () => { try { return JSON.parse(fs.readFileSync(SET_LOG, 'utf8')); } catch { return []; } };
const showVal = (v) => (v === undefined ? 'не задано' : String(v));

// экран советов перерисовывается часто — файл перечитываем не чаще раза в 2 секунды, перед правкой — всегда
let setCache = { at: 0, obj: null };
function settingsObj(fresh) {
  if (fresh || Date.now() - setCache.at > 2000) {
    let obj = null;
    try { obj = JSON.parse(fs.readFileSync(SETTINGS, 'utf8')); } catch {}
    setCache = { at: Date.now(), obj };
  }
  return setCache.obj;
}

const fixDone = (fix) => fix.changes.every((c) => getPath(settingsObj(), c.path) === c.to);

// что сделает скрипт: поставить недостающие значения или, если всё уже стоит и ставил ccwatch, вернуть прежние
function fixPlan(fix) {
  const obj = settingsObj(true);
  const todo = fix.changes.map((c) => ({ path: c.path, from: getPath(obj, c.path), to: c.to })).filter((c) => c.from !== c.to);
  if (todo.length) return { undo: false, changes: todo };
  const log = readSetLog();
  const back = [];
  for (const c of fix.changes) {
    const last = log.filter((e) => e.path.join('.') === c.path.join('.')).pop();
    if (last && last.to === c.to && last.from !== c.to) back.push({ path: c.path, from: c.to, to: last.from });
  }
  return back.length ? { undo: true, changes: back } : null; // null — уже настроено не ccwatch, трогать нечего
}

// шаги скрипта — их показываем перед подтверждением
function fixSteps(plan) {
  return ['сохранить копию ~/.claude/settings.json в settings.json.ccwatch-bak']
    .concat(plan.changes.map((c) => `в settings.json: ${c.path.join(' → ')}: ${showVal(c.from)} → ${showVal(c.to)}`))
    .map((x, k) => `${k + 1}. ${x}`);
}

function runFix(fix, plan) {
  let obj;
  try { obj = JSON.parse(fs.readFileSync(SETTINGS, 'utf8')); } catch (e) {
    return 'Не удалось прочитать settings.json (' + e.message + '), поменяйте вручную.';
  }
  for (const c of plan.changes) {
    let o = obj;
    for (const k of c.path.slice(0, -1)) { if (!o[k] || typeof o[k] !== 'object') o[k] = {}; o = o[k]; }
    const last = c.path[c.path.length - 1];
    if (c.to === undefined) delete o[last];
    else o[last] = c.to;
  }
  try {
    fs.copyFileSync(SETTINGS, SETTINGS + '.ccwatch-bak');
    const tmp = SETTINGS + '.ccwatch-tmp';
    fs.writeFileSync(tmp, JSON.stringify(obj, null, 2) + '\n');
    fs.renameSync(tmp, SETTINGS);
    fs.mkdirSync(BASE, { recursive: true });
    const at = Date.now();
    const log = readSetLog().concat(plan.changes.map((c) => ({ at, path: c.path, from: c.from, to: c.to }))).slice(-50);
    fs.writeFileSync(SET_LOG, JSON.stringify(log, null, 1));
  } catch (e) {
    return 'Не удалось записать settings.json: ' + e.message;
  }
  setCache.at = 0;
  return (plan.undo ? `Вернул как было: ${fix.what}.` : `Готово: ${fix.what}.`)
    + ' Обычно Claude Code подхватывает правку на лету, иначе — после перезапуска сессии.';
}

// Автоматическая просьба о плане нужна, только пока монитор открыт: при выходе её снимаем.
// Советы и просьбы, отправленные вручную, остаются ждать доставки — их хотели отправить.
function dropMyAsks() {
  for (const id of autoAsks) {
    const f = path.join(ASK_DIR, id + '.json');
    try {
      const r = JSON.parse(fs.readFileSync(f, 'utf8'));
      if (!r.autoPlan) continue;
      if (Array.isArray(r.texts) && r.texts.length) fs.writeFileSync(f, JSON.stringify({ ...r, plan: false, autoPlan: false, ttl: MANUAL_TTL }));
      else fs.unlinkSync(f);
    } catch {}
  }
  autoAsks.clear();
}

// просроченные запросы (у каждого свой срок) и отметки о доставке старше суток
function cleanAsks() {
  let names = [];
  try { names = fs.readdirSync(ASK_DIR); } catch {}
  for (const n of names) {
    const f = path.join(ASK_DIR, n);
    try {
      let ttl = AUTO_TTL;
      try { ttl = JSON.parse(fs.readFileSync(f, 'utf8')).ttl || AUTO_TTL; } catch {}
      if (Date.now() - fs.statSync(f).mtimeMs > ttl) fs.unlinkSync(f);
    } catch {}
  }
  try { names = fs.readdirSync(DONE_DIR); } catch { names = []; }
  for (const n of names) {
    const f = path.join(DONE_DIR, n);
    try { if (Date.now() - fs.statSync(f).mtimeMs > 24 * 3600e3) fs.unlinkSync(f); } catch {}
  }
}

function askLine(s, now) {
  const a = s.ask;
  if (!a || !a.plan) return null;
  if (a.state === 'wait') {
    return C.amber(`План запрошен ${ago(now - a.at)} назад, дойдёт при следующем действии агента.`)
      + (hook.installed ? '' : C.pink(' Хук не подключён!'));
  }
  if (s.planAt > a.at || now - a.at > 10 * 60e3) return null;
  return C.green(`Запрос передан ${ago(now - a.at)} назад, ждём список шагов.`);
}

// ── реестр живых сессий ─────────────────────────────────────────────────────

const sessions = new Map();

function alive(pid) {
  try { process.kill(pid, 0); return true; } catch (e) { return e.code === 'EPERM'; }
}

// ── где запущена сессия ─────────────────────────────────────────────────────
// Идём от процесса claude вверх по родителям: оболочка над ним — это вкладка терминала,
// первое знакомое приложение выше — где открыта вкладка, его заголовок — окно.

const APPS = [
  [/^code(\.exe)?$/i, 'VS Code'], [/^cursor(\.exe)?$/i, 'Cursor'], [/^windsurf(\.exe)?$/i, 'Windsurf'],
  [/^windowsterminal(\.exe)?$/i, 'Windows Terminal'], [/^claude(\.exe)?$/i, 'Claude (десктоп)'],
  [/^(idea|webstorm|pycharm|rider|goland|clion|phpstorm|rubymine)(64)?(\.exe)?$/i, 'JetBrains IDE'],
  [/^(iterm2|terminal|alacritty|kitty|wezterm|wezterm-gui|ghostty|warp|tabby|hyper|konsole|gnome-terminal-server|tmux)(\.exe)?$/i, null],
];
const SHELLS = [
  [/^bash(\.exe)?$/i, 'bash'], [/^pwsh(\.exe)?$/i, 'PowerShell 7'], [/^powershell(\.exe)?$/i, 'Windows PowerShell'],
  [/^cmd(\.exe)?$/i, 'cmd'], [/^zsh$/, 'zsh'], [/^fish$/, 'fish'], [/^sh$/, 'sh'], [/^wsl(\.exe)?$/i, 'WSL'],
];
const where = new Map(); // pid → {state: 'pending' | 'done', info, mem — память всего дерева процессов, procs, at}
let whereBusy = false;
let whereAt = 0;

function describeChain(chain) {
  let shell = null;
  let app = null;
  let title = '';
  const up = chain.slice(1); // [0] — сам claude
  for (let i = 0; i < up.length; i++) {
    const p = up[i];
    const sh = SHELLS.find(([re]) => re.test(p.name));
    if (sh) shell = { name: sh[1], started: Date.parse(p.started) || 0 }; // самая внешняя оболочка — вкладка
    const ap = APPS.find(([re]) => re.test(p.name));
    if (p.title && !title) title = p.title;
    if (!ap) continue;
    app = ap[1] || p.name.replace(/\.exe$/i, '');
    // у VS Code и похожих вкладкой владеет дочерний процесс, а заголовок окна — у главного над ним
    for (let j = i; j < up.length && !title && up[j].name === p.name; j++) title = up[j].title;
    break;
  }
  if (!app) app = shell ? 'отдельное окно консоли' : '';
  return { app, title, shell };
}

// По pid сессий: цепочка родителей (где запущена) и память всего дерева процессов (сам claude и
// всё, что он запустил, например MCP-серверы). Одна таблица процессов на все сессии; в живом режиме —
// в фоне раз в 30 секунд (PowerShell стартует секунду), в --once — сразу.
function refreshProcs(pids, sync) {
  const list = pids.filter(Boolean);
  if (!list.length || (whereBusy && !sync)) return;
  for (const p of list) if (!where.has(p)) where.set(p, { state: 'pending' });
  const finish = (map) => {
    whereBusy = false;
    whereAt = Date.now();
    for (const p of list) {
      const r = map[p];
      if (!r) { if (where.get(p).state === 'pending') where.set(p, { state: 'done', info: describeChain([]) }); continue; }
      where.set(p, { state: 'done', info: describeChain([].concat(r.chain || [])), mem: r.mem || 0, procs: r.procs || 0, at: whereAt });
    }
  };
  const { execFile, execFileSync } = require('child_process');
  if (process.platform === 'win32') {
    const script = "$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.Encoding]::UTF8; "
      + '$all=@{}; $kids=@{}; '
      + 'foreach ($p in Get-CimInstance Win32_Process) { $id=[int]$p.ProcessId; $all[$id]=$p; $pp=[int]$p.ParentProcessId; '
      + 'if ($pp -ne $id) { if (-not $kids.ContainsKey($pp)) { $kids[$pp]=New-Object System.Collections.ArrayList }; [void]$kids[$pp].Add($id) } }; '
      + `$r=@{}; foreach ($id in @(${list.join(',')})) { $chain=@(); $cur=$all[$id]; $i=0; `
      + 'while ($cur -and $i -lt 12) { $pr=Get-Process -Id $cur.ProcessId -ErrorAction SilentlyContinue; $t=""; if ($pr) { $t=$pr.MainWindowTitle }; '
      + "$chain += [pscustomobject]@{ name=$cur.Name; title=$t; started=$(if ($cur.CreationDate) { $cur.CreationDate.ToString('o') } else { '' }) }; "
      + '$cur=$all[[int]$cur.ParentProcessId]; $i++ }; '
      + '$mem=0; $n=0; $seen=@{}; $st=New-Object System.Collections.Stack; $st.Push($id); '
      + 'while ($st.Count) { $x=$st.Pop(); if ($seen.ContainsKey($x)) { continue }; $seen[$x]=1; '
      + 'if ($all[$x]) { $mem += $all[$x].WorkingSetSize; $n++ }; if ($kids[$x]) { foreach ($c in $kids[$x]) { $st.Push($c) } } }; '
      + '$r["$id"]=[pscustomobject]@{ chain=$chain; mem=$mem; procs=$n } }; '
      + '$r | ConvertTo-Json -Depth 5 -Compress';
    const args = ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')];
    const parse = (out) => { try { return JSON.parse(out) || {}; } catch { return {}; } };
    if (sync) {
      // stderr забираем себе: иначе служебный вывод PowerShell попадёт прямо в консоль
      try { finish(parse(execFileSync('powershell.exe', args, { encoding: 'utf8', windowsHide: true, timeout: 20000, stdio: ['ignore', 'pipe', 'pipe'] }))); } catch { finish({}); }
      return;
    }
    whereBusy = true;
    execFile('powershell.exe', args, { encoding: 'utf8', windowsHide: true, timeout: 20000 }, (err, out) => finish(parse(out)));
    return;
  }
  // macOS / Linux: одна таблица процессов, заголовков окон тут нет
  const fromPs = (out) => {
    const procs = new Map();
    const kids = new Map();
    for (const line of String(out).split('\n')) {
      const m = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\w{3} \w{3}\s+\d+ [\d:]+ \d{4})\s+(.+)$/.exec(line);
      if (!m) continue;
      const pid = Number(m[1]);
      const ppid = Number(m[2]);
      procs.set(pid, { ppid, rss: Number(m[3]) * 1024, started: new Date(m[4]).toISOString(), name: path.basename(m[5].trim()) });
      if (ppid !== pid) { if (!kids.has(ppid)) kids.set(ppid, []); kids.get(ppid).push(pid); }
    }
    const map = {};
    for (const p of list) {
      const chain = [];
      for (let cur = procs.get(p), i = 0; cur && i < 12; cur = procs.get(cur.ppid), i++) chain.push({ name: cur.name, title: '', started: cur.started });
      let mem = 0;
      let n = 0;
      const seen = new Set();
      for (const st = [p]; st.length;) {
        const x = st.pop();
        if (seen.has(x)) continue;
        seen.add(x);
        if (procs.has(x)) { mem += procs.get(x).rss; n++; }
        st.push(...(kids.get(x) || []));
      }
      map[p] = { chain, mem, procs: n };
    }
    return map;
  };
  const args = ['-A', '-o', 'pid=,ppid=,rss=,lstart=,comm='];
  if (sync) { try { finish(fromPs(execFileSync('ps', args, { encoding: 'utf8' }))); } catch { finish({}); } return; }
  whereBusy = true;
  execFile('ps', args, { encoding: 'utf8' }, (err, out) => finish(fromPs(out)));
}

const mb = (bytes) => Math.round(bytes / 1048576) + ' МБ';
const memText = (w) => (w && w.mem ? `процесс ${mb(w.mem)}` + (w.procs > 1 ? ` (с ${w.procs - 1} дочерними)` : '') : '');

// Завершить CLI-сессию: процесс claude со всеми дочерними. Транскрипт остаётся, сессию можно вернуть.
// Десктопные не трогаем: их процессами управляет приложение.
function killSession(s, done) {
  const r = s.reg || {};
  if (r.entrypoint === 'claude-desktop') return done('Десктопную сессию закройте в самом приложении (в архив): иначе оно перезапустит процесс или покажет ошибку.');
  if (!s.live || !r.pid) return done('Сессия уже не запущена.');
  const ok = `Сессия ${s.name} завершена. Вернуть: claude --resume ${s.id} в папке ${r.cwd}`;
  const { execFile, execFileSync } = require('child_process');
  if (process.platform === 'win32') {
    execFile('taskkill', ['/PID', String(r.pid), '/T', '/F'], { windowsHide: true }, (err) => done(err && alive(r.pid) ? 'Не удалось завершить: ' + err.message : ok));
    return;
  }
  try { execFileSync('pkill', ['-TERM', '-P', String(r.pid)], { stdio: 'ignore' }); } catch {}
  try { process.kill(r.pid, 'SIGTERM'); done(ok); } catch (e) { done('Не удалось завершить: ' + e.message); }
}

// строка «Запущена: …» для карточки сессии
function whereLine(s) {
  const r = s.reg || {};
  const since = r.startedAt ? 'сессия с ' + clock(r.startedAt) : '';
  const w = where.get(r.pid);
  if (r.entrypoint === 'claude-desktop') return ['приложение Claude (десктоп), вкладка Code', since, memText(w)].filter(Boolean).join(' · ');
  if (!s.live) return since;
  if (!w || w.state === 'pending') return ['ищу окно…', since].filter(Boolean).join(' · ');
  const { app, title, shell } = w.info;
  if (!app && !shell) return ['окно определить не удалось', since, memText(w)].filter(Boolean).join(' · ');
  const appText = app + (title && !title.startsWith(app) ? ` «${title.replace(/ - (Visual Studio Code|Cursor|Windsurf)$/, '')}»` : '');
  const termText = shell ? `терминал ${shell.name}` + (shell.started ? ` (открыт ${clock(shell.started)})` : '') : '';
  return [appText, termText, since, memText(w)].filter(Boolean).join(' · ');
}

function scanRegistry() {
  let names = [];
  try { names = fs.readdirSync(SESSIONS_DIR); } catch {}
  const seen = new Set();
  for (const n of names) {
    if (!/^\d+\.json$/.test(n)) continue;
    let r;
    try { r = JSON.parse(fs.readFileSync(path.join(SESSIONS_DIR, n), 'utf8')); } catch { continue; }
    if (!r.sessionId || !alive(r.pid)) continue;
    seen.add(r.sessionId);
    let s = sessions.get(r.sessionId);
    if (!s) sessions.set(r.sessionId, (s = new Session(r.sessionId)));
    if (BELL && s.reg && s.reg.status === 'busy' && r.status !== 'busy') process.stdout.write('\x07');
    s.reg = r;
    s.live = true;
  }
  for (const s of sessions.values()) if (!seen.has(s.id)) s.live = false;
}

// input — набираемое сообщение сессии {id, text}; setConfirm — ждём «y» на правку настроек {id, st, plan, until}
const ui = { view: 'list', sel: null, flash: null, planScroll: null, planPage: 10, tipSel: 0, input: null, setConfirm: null };

function visible() {
  return [...sessions.values()]
    .filter((s) => s.live || s.id === ui.sel)
    .sort((a, b) => ((b.reg && b.reg.startedAt) || 0) - ((a.reg && a.reg.startedAt) || 0));
}

function tick() {
  const now = Date.now();
  checkHook(now);
  scanRegistry();
  if (!ONCE) {
    // окно и память: сразу для новых сессий, дальше раз в 30 секунд
    const pids = [...sessions.values()].filter((s) => s.live && s.reg).map((s) => s.reg.pid);
    if (pids.some((p) => !where.has(p)) || now - whereAt > 30e3) refreshProcs(pids);
  }
  for (const s of sessions.values()) {
    if (!s.live && s.id !== ui.sel) continue;
    s.refresh(now);
    const r = s.reg || {};
    // десктоп-сессии обычно сами заводят план в первые секунды, поэтому ждём полминуты работы
    const busyFor = now - (r.statusUpdatedAt || now);
    if (AUTO_ASK && !ONCE && hook.installed && s.live && r.status === 'busy' && busyFor > 30e3 && !s.plan.length && !s.ask && !s.autoAsked) {
      s.autoAsked = true;
      askPlan(s, true);
    }
  }
  const list = visible();
  if (!list.some((s) => s.id === ui.sel)) ui.sel = list.length ? list[0].id : null;
}

// ── экраны ──────────────────────────────────────────────────────────────────

function header(W, now, note) {
  const left = '  ' + bold(C.pink('ccwatch')) + C.dim('  ·  ' + note);
  return spread(left, C.dim(new Date(now).toLocaleTimeString('ru-RU')) + '  ', W);
}

function footer(text, now) {
  const f = ui.flash && ui.flash.until > now ? '  ' + C.amber(ui.flash.text) : '';
  return '  ' + C.faint(text) + f;
}

function card(s, W, now, selected) {
  const g = selected ? ' ' + C.pink('▌') + ' ' : '   ';
  const inner = W - 4;
  const st = s.status(now);

  const name = clip(s.name, inner - 2 - vis(st.text) - 2);
  const l1 = spread(g + st.icon + ' ' + (selected ? bold(C.text(name)) : C.text(name)), st.text + ' ', W);

  const p = s.progress;
  let l2;
  if (!s.main) {
    l2 = g + '  ' + C.faint('транскрипт не найден');
  } else if (p.n) {
    const bw = Math.min(24, Math.max(8, Math.floor(inner / 4)));
    const what = p.cur ? p.cur.content : p.done === p.n ? 'все шаги выполнены' : '';
    const e = s.estimate(now);
    const right = !e ? '' : e.allDone ? C.dim(`за ${dur(e.spent)} при оценке ${approx(e.total)} `)
      : (e.over ? C.amber : C.cyan)(`осталось ${approx(e.left)} · ≈${clock(now + e.left)} `);
    const left = g + '  ' + bar(p.done / p.n, bw) + C.text(`${p.done}/${p.n}`.padStart(6)) + '  ';
    l2 = spread(left + C.dim(clip(what, W - vis(left) - vis(right) - 2)), right, W);
  } else {
    const a = s.activity;
    const act = a ? (a.who ? a.who + ' → ' : '') + describe(a) : '';
    const tag = s.ask && s.ask.plan ? (s.ask.state === 'wait' ? C.amber('план запрошен') : C.green('запрос передан')) : C.faint('без плана');
    l2 = g + '  ' + tag + (act ? '  ' + C.dim(clip(act, inner - 17)) : '');
  }

  const t = s.tokens;
  const run = s.running;
  const models = [...new Set(run.map((a) => shortModel(a.model)))].join(', ');
  const meta = [
    path.basename((s.reg && s.reg.cwd) || ''),
    shortModel(s.model),
    s.ctx ? 'контекст ' + tok(s.ctx) : '',
    `вых ${tok(t.o)} · вх ${tok(t.i)} · кэш ${tok(t.c)}`,
    run.length ? `агентов в работе ${run.length} (${models})` : '',
    (() => { const w = s.reg && where.get(s.reg.pid); return w && w.mem ? 'ОЗУ ' + mb(w.mem) : ''; })(),
  ].filter(Boolean).join(' · ');
  const l3 = g + '  ' + C.dim(clip(meta, inner - 2));
  return [l1, l2, l3];
}

function listView(W, H, now) {
  const list = visible();
  const body = [header(W, now, list.length ? `сессий: ${list.length}` : 'сессии Claude Code'), ''];
  if (!list.length) body.push('  ' + C.dim('Нет запущенных сессий Claude Code.'));
  const cards = list.map((s) => card(s, W, now, s.id === ui.sel));
  // если карточки не влезают — прокручиваем так, чтобы выбранная была видна
  const fits = Math.max(1, Math.floor((H - 3) / 4));
  const selIdx = Math.max(0, list.findIndex((s) => s.id === ui.sel));
  const from = Math.min(Math.max(0, selIdx - fits + 1), Math.max(0, cards.length - fits));
  cards.slice(from, from + fits).forEach((c, k) => { if (k) body.push(''); body.push(...c); });
  const range = cards.length > fits ? `  ·  ${from + 1}–${Math.min(from + fits, cards.length)} из ${cards.length}` : '';
  return { body, foot: footer('↑↓ выбор · Enter открыть · p запросить план · q выход' + range, now) };
}

// справа у шага: факт и оценка у сделанного, сколько идёт у текущего, оценка у будущих
function stepTime(x, now) {
  if (x.status === 'completed') {
    const fact = x.doneAt && x.startedAt && x.doneAt > x.startedAt ? dur(x.doneAt - x.startedAt) : '';
    return C.dim([fact, x.est ? approx(x.est) : ''].filter(Boolean).join(' / '));
  }
  if (x.status === 'in_progress') {
    const spent = x.startedAt ? now - x.startedAt : 0;
    const txt = [spent ? 'идёт ' + dur(spent) : '', x.est ? approx(x.est) : ''].filter(Boolean).join(' / ');
    return (x.est && spent > x.est ? C.amber : C.pink)(txt);
  }
  return x.est ? C.faint(approx(x.est)) : '';
}

const stepMark = (x) => (x.status === 'completed' ? C.green('✓') : x.status === 'in_progress' ? C.pink('▶') : C.faint('·'));
const stepText = (x, t) => (x.status === 'completed' ? C.dim(t) : x.status === 'in_progress' ? bold(C.pink(t)) : C.text(t));

// перенос по словам; слишком длинное слово режется
function wrap(text, w) {
  const out = [];
  let cur = '';
  for (const word of plain(text).split(' ').filter(Boolean)) {
    if (!cur) cur = word;
    else if (vis(cur) + 1 + vis(word) <= w) cur += ' ' + word;
    else { out.push(cur); cur = word; }
    while (vis(cur) > w) { out.push(chars(cur).slice(0, w).join('')); cur = chars(cur).slice(w).join(''); }
  }
  if (cur) out.push(cur);
  return out.length ? out : [''];
}

function detailView(s, W, H, now) {
  const inner = W - 4;
  const st = s.status(now);
  const r = s.reg || {};
  const head = [
    spread('  ' + C.dim('←') + ' ' + bold(C.text(clip(s.name, inner - vis(st.text) - 6))), st.icon + ' ' + st.text + ' ', W),
    '    ' + C.dim(clip([r.cwd, s.model, entryName(r.entrypoint), s.id.slice(0, 8),
      s.plan.length ? { text: 'план из меток в ответах', file: 'план из файла ~/.ccwatch/plans' }[s.planSource] : ''].filter(Boolean).join(' · '), inner)),
  ];
  const at = whereLine(s);
  if (at) wrap(at, inner - 11).forEach((l, k) => head.push('    ' + (k ? ' '.repeat(10) : C.dim('Запущена: ')) + C.text(l)));
  head.push('');

  // нижняя часть собирается первой: от неё зависит, сколько шагов плана влезет
  const tail = [''];
  const label = (t) => C.dim(pad(t, 12));
  if (s.last) {
    const when = C.faint(ago(now - s.last.at) + ' назад');
    tail.push('    ' + label(st.busy ? 'Сейчас' : 'Последнее') + C.text(clip(describe(s.last), inner - 14 - vis(when))) + '  ' + when);
  }
  const run = s.running;
  if (s.agentList.length) {
    tail.push('    ' + label('Агенты') + C.text(`${run.length} в работе · ${s.agentList.length} всего`));
    const shown = run.slice(0, 5);
    for (const a of shown) {
      const when = a.t.last ? C.faint(ago(now - a.t.last.at) + ' назад') : '';
      const info = `${a.name} · ${shortModel(a.model)} · ${ago(agentTime(a, now))}` + (a.t.last ? ' → ' + describe(a.t.last) : '');
      tail.push('    ' + ' '.repeat(12) + C.cyan('◆ ') + C.dim(clip(info, inner - 16 - vis(when))) + (when ? '  ' + when : ''));
    }
    if (run.length > shown.length) tail.push('    ' + ' '.repeat(12) + C.faint(`… и ещё ${run.length - shown.length}`));
  }
  const { active: tips, delivered } = s.activeTips(now);
  if (tips.length || delivered.size) {
    const price = (t) => (t.kind === 'time' ? `≈${dur(t.cost)}` : `≈${tok(t.w)}`);
    const head = tips.length ? `${tips.length} новых, первый — «${clip(tips[0].title, 40)}» ${price(tips[0])}` : 'новых нет';
    const sent = delivered.size ? `, отправлено ${delivered.size}` : '';
    tail.push('    ' + label('Советы') + C.amber(head + sent) + C.faint(' · s — открыть'));
  }
  const sk = s.skillUse();
  if (sk.bySkill.size) {
    const top = [...sk.bySkill].sort((a, b) => b[1].n - a[1].n).slice(0, 3).map(([n, r]) => `${n} ×${r.n}`).join(', ');
    tail.push('    ' + label('Скиллы') + C.text(clip(`агенты ${sk.agent}, вы ${sk.userSkills}: ${top}`, inner - 30)) + C.faint(' · i — история'));
  }
  tail.push('');

  const rows = s.rows(now);
  const total = rows.reduce((t, x) => ({
    label: 'итого', i: t.i + x.i, wi: t.wi + x.wi, c: t.c + x.c, o: t.o + x.o,
    agents: x.agents == null ? t.agents : (t.agents || 0) + x.agents,
    ms: x.ms == null ? t.ms : (t.ms || 0) + x.ms,
  }), { label: 'итого', i: 0, wi: 0, c: 0, o: 0, agents: null, ms: null });
  const lw = Math.min(28, Math.max(12, ...rows.map((x) => vis(x.label)))) + 2;
  const cell = (v) => String(v == null ? '—' : v).padStart(9);
  // доля — часть общей цены сессии: вход, кэш и выход сводятся к одной цене по их тарифам
  const cost = (x) => x.wi + x.c * PRICE.read + x.o * PRICE.output;
  const all = cost(total);
  const part = (v) => (all ? Math.round((v / all) * 100) + '%' : null);
  const tline = (x) => cell(tok(x.o)) + cell(tok(x.i)) + cell(tok(x.c)) + cell(part(cost(x))) + cell(x.agents) + cell(x.ms == null ? null : ago(x.ms));
  tail.push('    ' + C.faint(pad('Токены', lw) + ['выход', 'вход', 'из кэша', 'доля', 'агентов', 'время'].map((h) => h.padStart(9)).join('')));
  for (const x of rows) tail.push('    ' + C.dim(pad(x.label, lw)) + C.text(tline(x)));
  if (rows.length > 1) tail.push('    ' + C.dim(pad('итого', lw)) + bold(C.cyan(tline(total))));
  tail.push('    ' + C.dim(pad('контекст', lw)) + C.text(cell(tok(s.ctx))));
  if (all) {
    // памятка: за ×1 — самое дешёвое, чтение кэша; так видно, чего бояться
    const k = (v) => '×' + String(Math.round((v / PRICE.read) * 10) / 10).replace('.', ',');
    const parts = [
      { name: 'чтение кэша', v: total.c * PRICE.read, why: 'за токен оно самое дешёвое, но каждый запрос перечитывает весь контекст — бояться стоит его размера' },
      { name: 'вход и запись в кэш', v: total.wi, why: 'бояться стоит больших новых входов и перезаписи кэша после пауз' },
      { name: 'выход', v: total.o * PRICE.output, why: 'бояться стоит длинных ответов и размышлений' },
    ];
    const top = parts.slice().sort((a, b) => b.v - a.v)[0];
    const rates = `Цена токена, если чтение кэша ×1: вход ${k(PRICE.input)} · запись в кэш ${k(PRICE.write5m)} (на час ${k(PRICE.write1h)}) · выход ${k(PRICE.output)}`;
    tail.push('', ...wrap(rates, inner - 2).map((l) => '    ' + C.text(l)));
    const memo = `Здесь цена делится так: ${parts.map((p) => `${p.name} ${part(p.v)}`).join(' · ')}. Больше всего — ${top.name}: ${top.why}.`;
    tail.push(...wrap(memo, inner - 2).map((l) => '    ' + C.faint(l)));
  }

  const mid = [];
  const p = s.progress;
  const ask = askLine(s, now);
  // ваше сообщение сессии: набор (клавиша m) или судьба последнего отправленного — место под него вычитаем из плана
  const typing = ui.input && ui.input.id === s.id;
  const msg = typing ? null : msgLine(s, now);
  const extra = typing ? wrap(ui.input.text, inner - 20).length + 2 : msg ? 2 : 0;
  if (p.n) {
    const pct = Math.round((p.done / p.n) * 100);
    const lbl = p.cur ? `шаг ${p.idx} из ${p.n}` : p.done === p.n ? 'готово' : `${p.done} из ${p.n}`;
    const tailText = `  ${String(pct).padStart(3)}%   ${lbl}`;
    mid.push('    ' + bar(p.done / p.n, Math.max(10, Math.min(60, inner - vis(tailText) - 2))) + C.text(tailText));

    // оценка времени: итог по плану, сколько прошло и осталось, когда закончит
    const e = s.estimate(now);
    if (e) {
      const parts = e.allDone
        ? [`оценка ${approx(e.total)}`, `заняло ${dur(e.spent)}`]
        : [`оценка ${approx(e.total)}`, `прошло ${dur(e.spent)}`, `осталось ${approx(e.left)}`, `закончит ≈ ${clock(now + e.left)}`];
      if (e.missing) parts.push(`без оценки шагов: ${e.missing}`);
      mid.push('    ' + C.cyan(parts.join(' · ')));
      if (e.over && !e.allDone) mid.push('    ' + C.amber(`Шаг «${clip(e.over.step.content, 40)}» идёт ${dur(e.over.spent)} при оценке ${approx(e.over.step.est)}`));
    } else {
      mid.push('    ' + C.faint('Оценок времени у шагов нет.'));
    }
    mid.push('');

    const room = Math.max(3, H - 1 - head.length - mid.length - tail.length - (ask ? 2 : 0) - extra - 2);
    const steps = s.plan;
    const focus = p.cur ? p.idx - 1 : p.done - 1;
    const from = steps.length > room ? Math.min(Math.max(0, focus - Math.floor(room / 2)), steps.length - room) : 0;
    if (from > 0) mid.push('    ' + C.faint(`… ещё ${from} выше`));
    for (const x of steps.slice(from, from + room)) {
      const right = stepTime(x, now);
      const txt = clip(x.content, inner - 6 - vis(right));
      const left = '    ' + stepMark(x) + '  ' + stepText(x, txt);
      mid.push(right ? spread(left, right + '  ', W) : left);
    }
    const below = steps.length - from - room;
    if (below > 0 || from > 0) mid.push('    ' + C.faint((below > 0 ? `… ещё ${below} ниже · ` : '') + 'Enter — весь план'));
    if (ask) mid.push('', '    ' + ask);
  } else {
    mid.push('    ' + (ask || C.faint(AUTO_ASK && hook.installed
      ? 'План не ведётся. Если сессия поработает полминуты без плана, ccwatch попросит его сам; p — попросить сейчас.'
      : 'План не ведётся. Нажмите p, чтобы попросить сессию его составить.')));
  }

  if (msg) mid.push('', '    ' + msg);
  if (typing) {
    const lines = wrap(ui.input.text, inner - 20);
    if (ui.input.text.endsWith(' ')) lines[lines.length - 1] += ' '; // wrap съедает пробел, а курсор должен стоять после него
    mid.unshift(...lines.map((l, k) => '    ' + (k ? ' '.repeat(18) : C.cyan('Сообщение сессии: ')) + C.text(l)
      + (k === lines.length - 1 ? C.pink('▌') : '')),
    '    ' + C.faint('Enter — отправить · Esc — отмена. Агент получит его при следующем вызове инструмента.'), '');
  }

  // подтверждение завершения сессии (клавиша x)
  if (ui.confirm && ui.confirm.id === s.id && ui.confirm.until > now) {
    const warn = st.busy ? ' Она сейчас работает — текущий шаг оборвётся.' : '';
    mid.unshift(...wrap(`Завершить сессию ${s.name}? Процесс claude закроется вместе с дочерними, транскрипт останется: `
      + `вернуть можно командой claude --resume ${s.id}.${warn} y — да, любая другая клавиша — нет.`, inner - 2)
      .map((l) => '    ' + C.amber(l)), '');
  }
  const kill = r.entrypoint === 'claude-desktop' ? '' : 'x завершить · ';
  const keys = typing ? 'Enter отправить сообщение · Esc отмена'
    : `Esc назад · Enter весь план · s советы · i скиллы · p запросить план · m написать · ${kill}q выход`;
  return { body: head.concat(mid, tail), foot: footer(keys, now) };
}

// Весь план: каждый шаг с номером и полным текстом (длинный переносится), прокрутка ↑↓ / PgUp / PgDn.
function planView(s, W, H, now) {
  const inner = W - 4;
  const st = s.status(now);
  const p = s.progress;
  const e = s.estimate(now);
  const head = [
    spread('  ' + C.dim('←') + ' ' + bold(C.text('План · ' + clip(s.name, inner - vis(st.text) - 14))), st.icon + ' ' + st.text + ' ', W),
  ];
  const summary = [];
  if (p.n) summary.push(`${Math.round((p.done / p.n) * 100)}%`, p.cur ? `шаг ${p.idx} из ${p.n}` : `${p.done} из ${p.n}`);
  if (e && !e.allDone) summary.push(`оценка ${approx(e.total)}`, `осталось ${approx(e.left)}`, `закончит ≈ ${clock(now + e.left)}`);
  if (e && e.allDone) summary.push(`оценка ${approx(e.total)}`, `заняло ${dur(e.spent)}`);
  head.push('    ' + C.cyan(clip(summary.join(' · '), inner)), '');

  const lines = [];
  let curLine = 0;
  const num = String(s.plan.length).length;
  s.plan.forEach((x, k) => {
    const right = stepTime(x, now);
    const prefix = String(k + 1).padStart(num) + '  ';
    const width = Math.max(10, inner - num - 6 - (right ? vis(right) + 2 : 0));
    const parts = wrap(x.content, width);
    if (x.status === 'in_progress') curLine = lines.length;
    parts.forEach((part, j) => {
      const left = '    ' + (j ? ' '.repeat(num + 2) + '   ' : C.faint(prefix) + stepMark(x) + '  ') + stepText(x, part);
      lines.push(j === 0 && right ? spread(left, right + '  ', W) : left);
    });
  });
  if (!lines.length) lines.push('    ' + C.faint('План не ведётся.'));

  const rows = Math.max(3, H - head.length - 2);
  const max = Math.max(0, lines.length - rows);
  if (ui.planScroll == null) ui.planScroll = Math.max(0, curLine - Math.floor(rows / 3)); // при открытии — к текущему шагу
  ui.planScroll = Math.min(Math.max(0, ui.planScroll), max);
  ui.planPage = rows;
  const shown = lines.slice(ui.planScroll, ui.planScroll + rows);
  const pos = lines.length > rows ? `  ·  строки ${ui.planScroll + 1}–${ui.planScroll + shown.length} из ${lines.length}` : '';
  return { body: head.concat(shown), foot: footer('↑↓ прокрутка · PgUp/PgDn · Esc назад · q выход' + pos, now) };
}

// История скиллов: сводка по скиллам (кто и сколько раз) и лента вызовов, новые сверху.
function skillsView(s, W, H, now) {
  const inner = W - 4;
  const st = s.status(now);
  const u = s.skillUse();
  const head = [
    spread('  ' + C.dim('←') + ' ' + bold(C.text('Скиллы · ' + clip(s.name, inner - vis(st.text) - 16))), st.icon + ' ' + st.text + ' ', W),
    '    ' + C.cyan(clip(`агенты вызвали скиллы ${times(u.agent)} · вы набрали скиллов ${times(u.userSkills)}`
      + (u.builtins ? ` и встроенных команд ${u.builtins}` : ''), inner)),
    '',
  ];
  const lines = [];
  if (u.bySkill.size) {
    lines.push('    ' + bold(C.text('По скиллам')));
    for (const [name, r] of [...u.bySkill].sort((a, b) => b[1].n - a[1].n)) {
      const who = [...r.who].sort((a, b) => b[1] - a[1]).map(([w, n]) => `${w} ×${n}`).join(', ');
      lines.push('    ' + C.text(pad(clip(name, 30), 31)) + C.cyan(String(r.n).padStart(4) + '  ') + C.dim(clip(who, inner - 37)));
    }
    lines.push('');
  }
  lines.push('    ' + bold(C.text('История')) + C.faint(' · новые сверху'));
  if (!u.events.length) lines.push('    ' + C.faint('В логах этой сессии скиллов пока нет.'));
  for (const e of u.events) {
    const kind = e.by === 'user' ? (BUILTIN.has(e.name) ? C.faint('команда') : C.amber('/скилл ')) : C.green('скилл  ');
    const args = e.args ? ' ' + e.args.replace(/\s+/g, ' ') : '';
    lines.push('    ' + C.faint(clock(e.t).padStart(11) + '  ') + kind + ' ' + C.text(clip(e.name + args, 40)) + C.dim('  ' + clip(e.who, inner - 64)));
  }
  const rows = Math.max(3, H - head.length - 2);
  ui.skScroll = Math.min(Math.max(0, ui.skScroll || 0), Math.max(0, lines.length - rows));
  const pos = lines.length > rows ? `  ·  строки ${ui.skScroll + 1}–${Math.min(ui.skScroll + rows, lines.length)} из ${lines.length}` : '';
  return { body: head.concat(lines.slice(ui.skScroll, ui.skScroll + rows)), foot: footer('↑↓ прокрутка · Esc назад · q выход' + pos, now) };
}

// Советы по экономии токенов: что видно в логах сессии и сколько это примерно стоит.
function tipsView(s, W, H, now) {
  const inner = W - 4;
  const st = s.status(now);
  ui.tipSel = Math.min(Math.max(0, ui.tipSel || 0), Math.max(0, s.activeTips(now).active.length - 1));
  const body = [
    spread('  ' + C.dim('←') + ' ' + bold(C.text('Советы по экономии · ' + clip(s.name, inner - vis(st.text) - 28))), st.icon + ' ' + st.text + ' ', W),
    ...wrap('Считаются по логам сессии, без запросов к модели. ≈ — цена в токенах обычного входа '
      + '(если чтение кэша ×1, то вход ×10, запись в кэш ×12,5–20, выход ×50): порядок величины и сколько это стоит сейчас, сэкономить удастся часть.', inner)
      .map((l) => '    ' + C.faint(l)),
    '',
  ];
  const { active, delivered, effects } = s.activeTips(now);
  const pending = s.ask && s.ask.state === 'wait' ? s.ask.keys || [] : [];

  // скрипт совета ждёт подтверждения: что сделает по шагам и на что повлияет
  const sc = ui.setConfirm;
  const pre = []; // всегда под заголовком, даже если список прокручен
  if (sc && sc.id === s.id && sc.until > now) {
    const head = sc.plan.undo ? `Скрипт «вернуть как было»: ${sc.fix.what}` : `Скрипт: ${sc.fix.what} — ${sc.fix.note}`;
    pre.push(...wrap(head, inner - 2).map((l) => '    ' + bold(C.amber(l))));
    for (const st of fixSteps(sc.plan)) pre.push(...wrap(st, inner - 6).map((l, k) => '      ' + (k ? '   ' : '') + C.amber(l)));
    pre.push('    ' + C.amber('y — выполнить, любая другая клавиша — отмена. Вернуть можно тем же скриптом на этом совете.'), '');
  }

  // доставленные советы: ответ агента и эффект «до / после»; они не мешают в активном списке
  if (delivered.size) {
    body.push('    ' + bold(C.text('Отправленные советы')));
    for (const [key, t] of [...delivered].sort((a, b) => b[1] - a[1])) {
      const e = effects.get(key);
      const right = e ? e.verdict.paint(e.verdict.text) + ' ' : '';
      body.push(spread('    ' + C.text(TIP_TITLES[key] || key) + C.faint(` · доставлен ${ago(now - t)} назад`), right, W));
      const ack = s.acks.get(key);
      if (ack && ack.t >= t - 60e3) {
        const head = ack.ok ? C.green('ответ агента: принято') : C.amber('ответ агента: не применимо');
        body.push('      ' + head + C.dim(clip(ack.text ? ' — ' + ack.text : '', inner - 30)));
      } else body.push('      ' + C.faint('ответа агента пока нет: он появится в его следующем сообщении'));
      if (e) body.push('      ' + C.dim(clip(`${e.what}: ${e.fmt(e.before)} → ${e.fmt(e.after)} (после: ${e.n})`, inner - 4)));
      if (e && e.overall) body.push('      ' + C.faint(clip(`в целом контекст на запрос: ${tok(e.overall.before)} → ${tok(e.overall.after)}`, inner - 4)));
    }
    body.push('    ' + C.faint(clip('Эффект — корреляция: мера может сдвинуться и от смены фазы задачи.', inner)), '');
  }

  if (!active.length) body.push('    ' + C.dim(delivered.size ? 'Новых советов нет.' : 'Явных потерь не видно.'));
  let selLine = 0;
  active.forEach((t, k) => {
    // советы по скорости идут отдельной группой: их цена — минуты работы агентов, а не токены
    if (t.kind === 'time' && (k === 0 || active[k - 1].kind !== 'time')) {
      body.push('    ' + bold(C.text('Скорость работы')) + C.faint(' · цена — минуты работы агентов (у параллельных складываются)'));
    }
    const sel = k === ui.tipSel;
    if (sel) selLine = body.length;
    const g = sel ? '  ' + C.pink('▌') + ' ' : '    ';
    let tag = '';
    const done = t.fix && fixDone(t.fix);
    if (!t.send) tag = C.faint(done ? ' · для вас' : t.fix ? ' · для вас, есть скрипт' : ' · для вас, вручную');
    else if (pending.includes(t.key)) tag = C.green(` · отправлен, ждёт действия агента (до ${clock(s.ask.at + (s.ask.ttl || MANUAL_TTL))})`);
    else if (delivered.has(t.key)) tag = C.amber(` · уже отправлялся ${ago(now - delivered.get(t.key))} назад, эффекта не видно`);
    if (done) tag += C.green(' · скрипт выполнен, настройка стоит');
    const price = t.kind === 'time' ? `≈${dur(t.cost)} ` : `≈${tok(t.w)} `;
    body.push(spread(g + (sel ? bold(C.text(t.title)) : C.text(t.title)) + tag, bold(C.cyan(price)), W));
    const detail = t.kind === 'time' ? t.detail : `${tok(t.cost)} ${t.unit}. ${t.detail}`;
    for (const line of wrap(detail, inner - 4)) body.push(g + '  ' + C.dim(line));
    body.push('');
  });
  // если не влезает — сдвигаем так, чтобы выбранный совет был виден
  const rows = H - 2 - pre.length;
  const start = body.length > rows ? Math.min(Math.max(0, selLine - 4), body.length - rows) : 0;
  const shown = [body[0]].concat(pre, body.slice(start + 1));
  // что сделает Enter на выбранном совете: агенту — отправить, «для вас» — запустить скрипт
  const cur = active[ui.tipSel];
  const enter = !cur ? '' : cur.send ? 'Enter отправить совет · ' : cur.fix ? 'Enter выполнить скрипт · ' : '';
  const u = cur && cur.send && cur.fix ? 'u выполнить скрипт · ' : '';
  return { body: shown, foot: footer(`↑↓ выбор · ${enter}${u}a отправить все агенту · Esc назад · q выход`, now) };
}

function frame(W, H) {
  const now = Date.now();
  const s = sessions.get(ui.sel);
  if (ui.view === 'tips' && s) return tipsView(s, W, H, now);
  if (ui.view === 'skills' && s) return skillsView(s, W, H, now);
  if (ui.view === 'plan' && s) return planView(s, W, H, now);
  return ui.view === 'detail' && s ? detailView(s, W, H, now) : listView(W, H, now);
}

// ── запуск ──────────────────────────────────────────────────────────────────

cleanAsks();
tick();

const wanted = val('--session');
if (wanted) {
  const q = wanted.toLowerCase();
  let s = [...sessions.values()].find((x) => x.id.startsWith(q) || x.name.toLowerCase().includes(q));
  if (!s && /^[0-9a-f-]{36}$/.test(q)) { // завершённую сессию открываем по полному id прямо из транскрипта
    sessions.set(q, (s = new Session(q)));
    s.refresh(Date.now());
    s.subsAt = 0;
    s.refresh(Date.now() + 5000);
  }
  if (s) { ui.sel = s.id; ui.view = has('--plan') ? 'plan' : has('--tips') ? 'tips' : has('--skills') ? 'skills' : 'detail'; }
}

if (ONCE) {
  const W = process.stdout.columns || 100;
  const s = sessions.get(ui.sel);
  // в снимке ждать фона некогда: окно и память — сразу
  refreshProcs([...sessions.values()].filter((x) => x.live && x.reg).map((x) => x.reg.pid), true);
  const { body } = frame(W, 200);
  console.log(body.map((l) => clipAnsi(l, W)).join('\n'));
  process.exit(0);
}

let closed = false;
function restore() {
  if (closed) return;
  closed = true;
  dropMyAsks();
  process.stdout.write('\x1b[?25h\x1b[?1049l');
  try { process.stdin.setRawMode(false); } catch {}
}
function quit() { restore(); process.exit(0); }
// на Windows закрытие окна консоли приходит как SIGHUP; если процесс убьют жёстко,
// недоставленный запрос всё равно выбросит хук через 10 минут
process.on('SIGHUP', quit);
process.on('SIGTERM', quit);

function draw() {
  const W = Math.max(40, process.stdout.columns || 100);
  const H = Math.max(10, process.stdout.rows || 30);
  const { body, foot } = frame(W, H);
  const lines = body.slice(0, H - 2);
  while (lines.length < H - 1) lines.push('');
  lines.push(foot);
  process.stdout.write('\x1b[H' + lines.map((l) => clipAnsi(l, W) + '\x1b[K').join('\n') + '\x1b[J');
}

process.on('exit', restore);
process.on('SIGINT', quit);
process.on('uncaughtException', (e) => { restore(); console.error(e); process.exit(1); });
process.stdout.write('\x1b[?1049h\x1b[?25l');
process.stdout.on('resize', draw);

process.stdin.setRawMode(true);
process.stdin.setEncoding('utf8');
process.stdin.resume();
process.stdin.on('data', (k) => {
  // набираем сообщение сессии: все клавиши, кроме Enter и Esc, идут в текст (q тоже)
  if (ui.input) {
    if (k === '\x03') return quit();
    const s = sessions.get(ui.input.id);
    if (k === '\x1b') { ui.input = null; ui.flash = { text: 'Сообщение не отправлено.', until: Date.now() + 3000 }; }
    else if (k === '\r' || k === '\n') {
      const text = ui.input.text.trim();
      ui.input = null;
      if (text && s) ui.flash = { text: sendMessage(s, text), until: Date.now() + 8000 };
    } else if (k === '\x7f' || k === '\b') ui.input.text = chars(ui.input.text).slice(0, -1).join('');
    else if (k === '\x15') ui.input.text = ''; // Ctrl+U — стереть всё
    else if (!k.startsWith('\x1b')) ui.input.text += k.replace(/\r\n?|\n/g, ' ').replace(/[\x00-\x1f\x7f]/g, '');
    draw();
    return;
  }
  // скрипт совета ждёт подтверждения: y — выполнить, любая другая клавиша — отмена
  if (ui.setConfirm) {
    const c = ui.setConfirm;
    ui.setConfirm = null;
    if (c.until > Date.now() && (k === 'y' || k === 'н' || k === 'д')) ui.flash = { text: runFix(c.fix, c.plan), until: Date.now() + 12000 };
    else ui.flash = { text: 'Настройка не изменена.', until: Date.now() + 3000 };
    draw();
    return;
  }
  if (k === '\x03' || k === 'q' || k === 'й') return quit();
  const up = k === '\x1b[A' || k === 'k';
  const down = k === '\x1b[B' || k === 'j';
  const open = k === '\r' || k === '\n' || k === ' ' || k === '\x1b[C' || k === 'f' || k === 'а';
  const back = k === '\x1b' || k === '\x1b[D' || k === '\x7f' || k === '\b';
  // ждём подтверждения завершения сессии: y — завершить, любая другая клавиша — отмена
  if (ui.confirm) {
    const c = ui.confirm;
    ui.confirm = null;
    const s = sessions.get(c.id);
    if (s && c.until > Date.now() && (k === 'y' || k === 'н' || k === 'д')) {
      ui.flash = { text: 'Завершаю…', until: Date.now() + 10000 };
      killSession(s, (msg) => { ui.flash = { text: msg, until: Date.now() + 15000 }; draw(); });
    } else ui.flash = { text: 'Завершение отменено.', until: Date.now() + 3000 };
    draw();
    return;
  }
  if (k === 'p' || k === 'з') ui.flash = { text: askPlan(sessions.get(ui.sel)), until: Date.now() + 5000 };
  else if (ui.view === 'list') {
    // сессию выбираем только в списке: в карточке стрелки ничего не переключают
    const list = visible();
    const i = Math.max(0, list.findIndex((s) => s.id === ui.sel));
    if ((up || down) && list.length) ui.sel = list[(i + (up ? -1 : 1) + list.length) % list.length].id;
    else if (open && list.length) ui.view = 'detail';
  } else if (ui.view === 'detail') {
    if (open) { ui.view = 'plan'; ui.planScroll = null; }
    else if (k === 's' || k === 'ы') { ui.view = 'tips'; ui.tipSel = 0; }
    else if (k === 'i' || k === 'ш') { ui.view = 'skills'; ui.skScroll = 0; }
    else if (k === 'm' || k === 'ь') {
      const s = sessions.get(ui.sel);
      if (s && s.live) ui.input = { id: s.id, text: '' };
      else ui.flash = { text: 'Сессия не запущена: сообщение некому передать.', until: Date.now() + 4000 };
    } else if (k === 'x' || k === 'ч') {
      const s = sessions.get(ui.sel);
      if (s && s.reg && s.reg.entrypoint === 'claude-desktop') killSession(s, (msg) => { ui.flash = { text: msg, until: Date.now() + 8000 }; });
      else if (s && s.live) ui.confirm = { id: s.id, until: Date.now() + 15000 };
    } else if (back) ui.view = 'list';
  } else if (ui.view === 'tips') {
    const s = sessions.get(ui.sel);
    const tips = s ? s.activeTips(Date.now()).active : []; // тот же список, что на экране
    if (up) ui.tipSel--;
    else if (down) ui.tipSel++;
    else if (k === '\r' || k === '\n' || k === 'u' || k === 'г') {
      // Enter: совет агенту — отправить, совет «для вас» — показать его скрипт; u — скрипт у любого совета
      const t = tips[ui.tipSel];
      const script = t && t.fix && (k === 'u' || k === 'г' || !t.send);
      if (script) {
        const plan = fixPlan(t.fix);
        if (plan) ui.setConfirm = { id: s.id, fix: t.fix, plan, until: Date.now() + 60000 };
        else ui.flash = { text: 'Настройка уже стоит, и ставил её не ccwatch: скрипту делать нечего.', until: Date.now() + 5000 };
      } else if (t && (k === '\r' || k === '\n')) {
        ui.flash = { text: t.send ? sendTip(s, t) : 'Этот совет — вручную: скрипта для него нет, что сделать, написано в описании.', until: Date.now() + 5000 };
      }
    } else if ((k === 'a' || k === 'ф') && s) ui.flash = { text: sendAllTips(s, tips), until: Date.now() + 6000 };
    else if (back) ui.view = 'detail';
  } else if (ui.view === 'skills') {
    if (up) ui.skScroll = (ui.skScroll || 0) - 1;
    else if (down) ui.skScroll = (ui.skScroll || 0) + 1;
    else if (k === '\x1b[5~') ui.skScroll = (ui.skScroll || 0) - 10;
    else if (k === '\x1b[6~') ui.skScroll = (ui.skScroll || 0) + 10;
    else if (back) ui.view = 'detail';
  } else if (ui.view === 'plan') {
    const page = Math.max(1, (ui.planPage || 10) - 1);
    if (up) ui.planScroll--;
    else if (down) ui.planScroll++;
    else if (k === '\x1b[5~') ui.planScroll -= page;
    else if (k === '\x1b[6~') ui.planScroll += page;
    else if (k === '\x1b[H' || k === '\x1b[1~') ui.planScroll = 0;
    else if (k === '\x1b[F' || k === '\x1b[4~') ui.planScroll = 1e9;
    else if (back) ui.view = 'detail';
  }
  draw();
});

setInterval(tick, 1000);
setInterval(draw, 120);
draw();
