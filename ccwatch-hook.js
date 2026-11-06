#!/usr/bin/env node
/*
 * Хук PostToolUse для ccwatch. Пока монитор открыт, он кладёт запрос «покажи план» (и выбранные
 * пользователем советы по экономии токенов) в
 * ~/.ccwatch/ask/<sessionId>.json, а хук на ближайшем вызове инструмента этой сессии
 * передаёт его агенту через additionalContext и отмечает доставку в ~/.ccwatch/done/.
 * Агент без инструмента списка задач один раз пишет план прямо в тексте ответа и дальше
 * ставит короткие строки-метки, ccwatch читает их из транскрипта.
 *
 * Подключение в ~/.claude/settings.json (Windows; ccwatch-hook.cmd запускает Node,
 * только когда есть запрос, а async не даёт хуку задерживать агента):
 *   "hooks": { "PostToolUse": [{ "hooks": [{ "type": "command", "command": "cmd",
 *     "args": ["/d", "/c", "C:\\Users\\<you>\\Desktop\\projects\\ccwatch\\ccwatch-hook.cmd"],
 *     "async": true }] }] }
 * На macOS и Linux: "command": "node", "args": ["/путь/к/ccwatch-hook.js"], "async": true.
 */
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');

const BASE = process.env.CCWATCH_DIR || path.join(os.homedir(), '.ccwatch');
const ASK_DIR = path.join(BASE, 'ask');
const DONE_DIR = path.join(BASE, 'done');
const TTL = 10 * 60e3; // запрос старше 10 минут уже никто не ждёт: монитор, скорее всего, закрыт

const TEXT = 'Пользователь открыл монитор ccwatch и хочет видеть план этой сессии с оценкой времени. '
  + 'Называй шаги понятным языком по сути работы, как для человека со стороны («Баланс механики яда», «Тестирование боя»), '
  + 'а не внутренними кодами вроде «W6: HT1»; код, если он нужен, пиши в скобках после названия. '
  + 'Если ты ведёшь план инструментом списка задач (update_plan, TodoWrite или TaskCreate/TaskUpdate), обнови его один раз в таком виде, '
  + 'дописав в конец каждого шага оценку времени вида «~15м» или «~1ч 30м», и продолжай как обычно. '
  + 'Если такого инструмента нет, то в своём следующем сообщении, прямо перед очередным вызовом инструмента, напиши текстом '
  + 'отдельную строку «ccwatch: план», под ней все шаги задачи с номерами и оценкой в конце («1. Баланс механики яда (BAL4) ~20м»), '
  + 'а затем строку «ccwatch: ✓1 ▶2» с отметкой сделанного (✓) и текущего шага (▶). '
  + 'Дальше в том сообщении, где берёшься за следующий шаг, добавляй к тексту одну строку вида «ccwatch: ✓2 ▶3». '
  + 'Отдельных вызовов инструментов ради плана не делай и работу не прерывай.';

let raw = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (c) => { raw += c; });
process.stdin.on('end', () => {
  let input;
  try { input = JSON.parse(raw); } catch { return; }
  // запрос адресован главному агенту, вызовы инструментов субагентов пропускаем
  if (!input.session_id || input.agent_id) return;
  const id = String(input.session_id).replace(/[^\w-]/g, '');
  const file = path.join(ASK_DIR, id + '.json');
  let req;
  try {
    req = JSON.parse(fs.readFileSync(file, 'utf8'));
    fs.unlinkSync(file);
  } catch {
    return;
  }
  // у запроса свой срок: автоматическая просьба о плане — 10 минут, отправленное вручную — до 12 часов
  if (Date.now() - (req.at || 0) > (req.ttl || TTL)) return;
  // в запросе может быть просьба о плане (plan) и советы по экономии токенов (texts);
  // старые запросы без полей — это просьба о плане
  const texts = Array.isArray(req.texts) ? req.texts.filter((t) => typeof t === 'string' && t) : [];
  const plan = req.plan === true || (req.plan === undefined && !texts.length);
  const parts = (plan ? [TEXT] : []).concat(texts);
  if (!parts.length) return;
  try {
    fs.mkdirSync(DONE_DIR, { recursive: true });
    fs.writeFileSync(path.join(DONE_DIR, id + '.json'), JSON.stringify({ ...req, deliveredAt: Date.now() }));
  } catch {}
  process.stdout.write(JSON.stringify({
    hookSpecificOutput: { hookEventName: 'PostToolUse', additionalContext: parts.join('\n\n') },
  }));
});
