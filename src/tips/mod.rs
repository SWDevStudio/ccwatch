//! Советы по экономии токенов и скорости: что попросить у агента, сколько это стоит и помогло ли.
//! Считаются локально по транскриптам; пересчёт не чаще раза в 2 секунды (у больших сессий десятки тысяч вызовов).

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::fixes::Fix;
use crate::requests::IDEAS_KEY;
use crate::session::Session;

pub mod effects;
pub mod skills;
pub mod speed;
mod tokens;

pub use effects::{verdict, Effect, Fmt, Verdict};
pub use skills::{is_builtin, SkillCount, SkillEvent, SkillReport};

/// Вид совета: токены (цена в токенах) или время (цена в миллисекундах работы агентов).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Tokens,
    Time,
    /// совет о самой программе (хук ccwatch): цены нет, агенту не отправить
    Setup,
}

#[derive(Clone, Debug)]
pub struct Tip {
    pub key: String,
    pub kind: Kind,
    pub title: String,
    pub cost: f64,
    /// Единица цены; у советов по времени — пустая строка.
    pub unit: &'static str,
    /// Можно ли отправить совет агенту (Enter).
    pub send: bool,
    /// Цена в обычных токенах для сортировки; None — посчитать по единице.
    pub w: Option<f64>,
    pub detail: String,
    /// Текст для агента; пустая строка — текста нет.
    pub text: String,
    /// Готовая настройка, которую можно записать в settings.json (клавиша u).
    pub fix: Option<Fix>,
}

pub struct ActiveTips {
    pub active: Vec<Tip>,
    pub delivered: IndexMap<String, f64>,
    pub effects: HashMap<String, Effect>,
}

/// Заголовки советов для экрана; у неизвестного ключа заголовка нет.
pub fn tip_title(key: &str) -> Option<&'static str> {
    Some(match key {
        "thinking" => "Размышления",
        "heavy" => "Большие выводы в контексте",
        "shared" => "Общие файлы у субагентов",
        "repeat" => "Повторы команд",
        "bloated" => "Разросшиеся субагенты",
        "shell" => "Механика у главного агента",
        "slowcheck" => "Долгие проверки",
        "smallshell" => "Мелкие команды оболочки",
        "polling" => "Ожидание в цикле",
        "recheck" => "Повторные проверки на том же коде",
        "timeout" => "Команды упираются в тайм-аут",
        "ramp" => "Субагенты долго разгоняются",
        _ => return None,
    })
}

/// Ваше сообщение сессии (клавиша m) идёт тем же хуком, что советы, но с ключом msg…
pub fn is_msg(key: &str) -> bool {
    crate::re!(r"^msg[0-9a-z]+$").is_match(key)
}

/// Служебные ключи хука, которые не советы: ваши сообщения и вопрос об идеях для анализатора.
pub fn is_service_key(key: &str) -> bool {
    is_msg(key) || key == IDEAS_KEY
}

/// Просьба к агенту ответить на совет одной строкой — без отдельных действий.
pub fn ack_request(key: &str) -> String {
    format!(
        " В ответ добавь к тексту своего следующего сообщения одну строку «ccwatch: принято {key} — что именно меняешь» \
         или «ccwatch: не применимо {key} — почему». Отдельных действий ради ответа не делай и текущую работу не прерывай."
    )
}

/// Заглушка в конце текста совета: на её месте подставляется просьба ответить строкой.
pub(super) const TAIL: &str = "{{ОТВЕТ}}";

/// Начало текста совета; topic — о чём совет («экономии токенов» или «скорости работы»).
pub(super) fn lead(key: &str, topic: &str) -> String {
    format!("Совет от монитора ccwatch (ccwatch:{key}) по {topic}, пользователь просит его учесть: ")
}

impl Session {
    /// Все советы: сначала по токенам (по убыванию цены), затем по времени.
    /// Пересчёт не чаще раза в 2 секунды.
    pub fn tips(&self, now: f64) -> Vec<Tip> {
        let cached =
            self.tips_cache.borrow().as_ref().filter(|(at, _)| now - at < 2000.0).map(|(_, list)| list.clone());
        if let Some(list) = cached {
            return list;
        }

        let mut all = self.token_tips();
        all.extend(self.speed_tips());
        // агент отвечает строкой в обычном тексте — ccwatch прочитает её из транскрипта
        for t in all.iter_mut() {
            if !t.text.is_empty() {
                t.text = t.text.replace(TAIL, &ack_request(&t.key));
            }
        }
        *self.tips_cache.borrow_mut() = Some((now, all.clone()));
        all
    }

    /// Советы, которые стоит показать: ещё не доставленные и доставленные, но без видимого эффекта.
    /// Доставленные с эффектом (или пока без данных) уходят в «Отправленные», чтобы не мозолить глаза.
    pub fn active_tips(&self, now: f64) -> ActiveTips {
        let mut delivered: IndexMap<String, f64> = IndexMap::new();
        if let Some(main) = &self.main {
            for d in &main.deliveries {
                if is_service_key(&d.key) {
                    continue; // ваши сообщения и вопрос об идеях — не советы
                }
                let later = delivered.get(&d.key).map_or(true, |&old| d.t > old);
                if later {
                    delivered.insert(d.key.clone(), d.t);
                }
            }
        }
        let effects: HashMap<String, Effect> = self.effects(now).into_iter().map(|e| (e.key.clone(), e)).collect();
        let active = self
            .tips(now)
            .into_iter()
            .filter(|t| {
                if !delivered.contains_key(&t.key) {
                    return true;
                }
                effects.get(&t.key).is_some_and(|e| e.enough && !e.verdict.text.starts_with("помогло"))
            })
            .collect();
        ActiveTips { active, delivered, effects }
    }
}
