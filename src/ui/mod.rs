//! Интерфейс: состояние экранов, общий каркас кадра (шапка, подвал, выбор экрана).
//! Сами экраны — в соседних файлах; клавиши — в keys.rs, живой цикл отрисовки — в live.rs.

use crate::app::App;
use crate::clock;
use crate::fixes::{Fix, FixPlan};
use crate::style::{bold, c};
use crate::text::spread;

pub mod detail;
pub mod ideas;
pub mod keys;
pub mod list;
pub mod live;
pub mod plan;
pub mod skills;
pub mod tips;

/// Экран монитора: список, карточка сессии, полный план, история скиллов, советы, идеи для анализатора.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    List,
    Detail,
    Plan,
    Skills,
    Tips,
    Ideas,
}

/// Короткое сообщение в подвале до срока `until` (мс от 1970).
pub struct Flash {
    pub text: String,
    pub until: f64,
}

/// Набираемое сообщение сессии `id`.
pub struct Input {
    pub id: String,
    pub text: String,
}

/// Скрипт совета ждёт «y» перед правкой settings.json.
pub struct SetConfirm {
    pub id: String,
    pub fix: Fix,
    pub plan: FixPlan,
    pub until: f64,
}

/// Завершение CLI-сессии ждёт «y».
pub struct KillConfirm {
    pub id: String,
    pub until: f64,
}

/// Состояние интерфейса: что открыто, что выбрано, что ждёт подтверждения.
pub struct Ui {
    pub view: View,
    /// id выбранной сессии
    pub sel: Option<String>,
    pub flash: Option<Flash>,
    /// прокрутка плана; None — ещё не трогали (экран сам выберет место)
    pub plan_scroll: Option<i64>,
    /// строк на страницу плана (PgUp / PgDn)
    pub plan_page: i64,
    pub tip_sel: i64,
    pub sk_scroll: i64,
    /// прокрутка экрана идей (строки)
    pub ideas_scroll: i64,
    pub input: Option<Input>,
    pub set_confirm: Option<SetConfirm>,
    pub confirm: Option<KillConfirm>,
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            view: View::List,
            sel: None,
            flash: None,
            plan_scroll: None,
            plan_page: 10,
            tip_sel: 0,
            sk_scroll: 0,
            ideas_scroll: 0,
            input: None,
            set_confirm: None,
            confirm: None,
        }
    }

    /// Показать сообщение в подвале на `ms` миллисекунд.
    pub fn flash(&mut self, text: impl Into<String>, ms: f64) {
        self.flash = Some(Flash { text: text.into(), until: clock::now() + ms });
    }
}

/// Кадр экрана: строки тела и строка подвала.
pub struct Frame {
    pub body: Vec<String>,
    pub foot: String,
}

/// Шапка: название слева, время справа; note — что сейчас показываем.
pub fn header(w: i64, now: f64, note: &str) -> String {
    let left = format!("  {}{}", bold(c::pink("ccwatch")), c::dim(format!("  ·  {note}")));
    let right = format!("{}  ", c::dim(clock::time_of_day(now)));
    spread(&left, &right, w)
}

/// Подвал: подсказка по клавишам и, пока не истекло, сообщение.
pub fn footer(ui: &Ui, text: &str, now: f64) -> String {
    let flash = match &ui.flash {
        Some(f) if f.until > now => format!("  {}", c::amber(&f.text)),
        _ => String::new(),
    };
    format!("  {}{}", c::faint(text), flash)
}

impl App {
    /// Кадр текущего экрана: карточка выбранной сессии, если экран требует сессию, иначе список.
    pub fn frame(&mut self, w: i64, h: i64) -> Frame {
        let now = clock::now();
        let view = self.ui.view;
        if let Some(i) = self.sel_index() {
            let s = &self.m.sessions[i];
            match view {
                View::Tips => return tips::tips_view(&self.m, &mut self.ui, s, w, h, now),
                View::Skills => return skills::skills_view(&mut self.ui, s, w, h, now),
                View::Plan => return plan::plan_view(&mut self.ui, s, w, h, now),
                View::Detail => return detail::detail_view(&self.m, &self.ui, s, w, h, now),
                View::Ideas => return ideas::ideas_view(&self.m, &mut self.ui, s, w, h, now),
                View::List => {}
            }
        }
        list::list_view(&self.m, &self.ui, w, h, now)
    }
}
