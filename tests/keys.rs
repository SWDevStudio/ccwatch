//! Клавиши монитора: переходы между экранами, набор сообщения, запрос плана и скрипт совета.
//! Данные — во временной папке теста (ccwatch-test-<pid>-<имя>); настоящие ~/.claude и ~/.ccwatch не трогаем.

use std::fs;
use std::path::{Path, PathBuf};

use ccwatch::app::App;
use ccwatch::cli;
use ccwatch::clock;
use ccwatch::fixes::{Change, Fix};
use ccwatch::paths::Paths;
use ccwatch::ui::keys::Key;
use ccwatch::ui::live::map_key;
use ccwatch::ui::{SetConfirm, View};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};

/// id сессии в тестовом реестре
const SESSION_ID: &str = "3f2c9a10-7d4e-4b8a-9c1d-0e5f6a7b8c9d";
/// папка сессии; транскрипт лежит в projects/<папка>, где всё, кроме букв и цифр, заменено на «-»
const CWD: &str = "C:\\work\\demo-project";
const PROJECT_DIR: &str = "C--work-demo-project";

/// Временная папка теста; удаляется при выходе из теста, даже если тест упал.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let dir = std::env::temp_dir().join(format!("ccwatch-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("временная папка теста");
        Sandbox { dir }
    }

    /// Папка данных Claude Code внутри песочницы.
    fn root(&self) -> PathBuf {
        self.dir.join("claude")
    }

    /// Папка ccwatch внутри песочницы.
    fn base(&self) -> PathBuf {
        self.dir.join("ccwatch")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Монитор с одной живой сессией: реестр с pid текущего процесса, транскрипт с планом,
/// settings.json с подключённым хуком.
fn start(sb: &Sandbox) -> App {
    let root = sb.root();
    let sessions = root.join("sessions");
    let project = root.join("projects").join(PROJECT_DIR);
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&project).unwrap();

    let now = clock::now() as i64;
    let reg = json!({
        "pid": std::process::id(),
        "sessionId": SESSION_ID,
        "cwd": CWD,
        "name": "демо-сессия",
        "status": "busy",
        "entrypoint": "cli",
        "startedAt": now - 600_000,
        "statusUpdatedAt": now - 60_000,
    });
    fs::write(sessions.join(format!("{}.json", std::process::id())), reg.to_string()).unwrap();

    let usage = json!({"input_tokens": 120, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 40});
    let lines = [
        json!({
            "type": "assistant", "uuid": "u1", "timestamp": "2026-10-18T10:00:00.000Z",
            "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": usage,
                "content": [{"type": "text", "text": "Начинаю"}]}
        }),
        json!({
            "type": "assistant", "uuid": "u2", "timestamp": "2026-10-18T10:00:05.000Z",
            "message": {"id": "msg_2", "model": "claude-opus-5-5", "usage": usage,
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "mcp__ccd_turn__update_plan",
                    "input": {"items": [
                        {"content": "Разобрать конфиг ~15м", "status": "completed"},
                        {"content": "Собрать тесты ~15м", "status": "in_progress"},
                        {"content": "Проверить сборку ~15м", "status": "pending"}
                    ]}}]}
        }),
    ];
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(project.join(format!("{SESSION_ID}.jsonl")), text).unwrap();

    // хук подключён на ccwatch.exe: советов о программе тогда нет, как и у пользователя после переключения
    let hook = json!({"type": "command", "command": "ccwatch.exe", "args": ["hook"], "async": true});
    let settings = json!({"hooks": {"PostToolUse": [{"hooks": [hook]}]}, "modelSettings": {}});
    fs::write(root.join("settings.json"), settings.to_string()).unwrap();

    let paths = Paths::new(&root, &sb.base());
    let opts = cli::parse(&["--once".to_string()]);
    let app = App::start(paths, opts, true);
    assert_eq!(app.m.sessions.len(), 1, "сессия из реестра должна появиться");
    assert_eq!(app.ui.sel.as_deref(), Some(SESSION_ID));
    app
}

/// Текст последнего сообщения в подвале (пусто, если его нет).
fn flash(app: &App) -> String {
    app.ui.flash.as_ref().map(|f| f.text.clone()).unwrap_or_default()
}

/// Читает JSON-файл из песочницы.
fn read_json(file: &Path) -> Value {
    let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    serde_json::from_str(&text).unwrap()
}

/// Файл запроса сессии в ccwatch/ask.
fn ask_file(sb: &Sandbox) -> PathBuf {
    sb.base().join("ask").join(format!("{SESSION_ID}.json"))
}

/// Правка для скрипта совета: кэш субагентов на час.
fn cache_fix() -> Fix {
    Fix {
        what: "кэш субагентов на час".into(),
        note: String::new(),
        changes: vec![Change { path: vec!["subagentPromptCacheTtl".into()], to: json!("1h"), label: None }],
    }
}

#[test]
fn enter_opens_card_and_esc_returns_to_list() {
    let sb = Sandbox::new("enter-esc");
    let mut app = start(&sb);
    assert_eq!(app.ui.view, View::List);
    assert!(!app.on_key(Key::Enter));
    assert_eq!(app.ui.view, View::Detail);
    assert!(!app.on_key(Key::Esc));
    assert_eq!(app.ui.view, View::List);
    // все экраны должны рисоваться без паники
    let _ = app.frame(100, 40);
}

#[test]
fn q_and_ctrl_c_quit_from_list_and_card() {
    let sb = Sandbox::new("quit");
    let mut app = start(&sb);
    assert!(app.on_key(Key::Char('q')), "q в списке выходит");
    assert!(app.on_key(Key::CtrlC), "Ctrl+C в списке выходит");
    assert!(!app.on_key(Key::Enter));
    assert!(app.on_key(Key::Char('q')), "q в карточке выходит");
    assert!(app.on_key(Key::CtrlC), "Ctrl+C в карточке выходит");
}

#[test]
fn control_chars_from_conpty_map_to_ctrl_keys() {
    let c3 = KeyEvent::new(KeyCode::Char('\u{3}'), KeyModifiers::NONE);
    let c15 = KeyEvent::new(KeyCode::Char('\u{15}'), KeyModifiers::NONE);
    assert_eq!(map_key(c3), Key::CtrlC);
    assert_eq!(map_key(c15), Key::CtrlU);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(map_key(ctrl_c), Key::CtrlC);
    // Ctrl+буква при русской раскладке: Ctrl+C — «с», Ctrl+U — «г», в любом регистре
    for c in ['с', 'С', 'C'] {
        assert_eq!(map_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)), Key::CtrlC, "{c}");
    }
    for c in ['г', 'Г', 'U'] {
        assert_eq!(map_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)), Key::CtrlU, "{c}");
    }
    // Ctrl+Q не выход из монитора: клавиша остаётся Other
    let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert_eq!(map_key(ctrl_q), Key::Other);
}

#[test]
fn card_enter_opens_plan_and_esc_goes_back() {
    let sb = Sandbox::new("plan-screen");
    let mut app = start(&sb);
    app.on_key(Key::Enter);
    app.on_key(Key::Enter);
    assert_eq!(app.ui.view, View::Plan);
    let _ = app.frame(100, 40);
    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);
}

#[test]
fn card_tips_and_skills_keys_latin_and_russian() {
    let sb = Sandbox::new("tips-skills");
    let mut app = start(&sb);
    app.on_key(Key::Enter);

    app.on_key(Key::Char('s'));
    assert_eq!(app.ui.view, View::Tips);
    let _ = app.frame(100, 40);
    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);

    app.on_key(Key::Char('ы'));
    assert_eq!(app.ui.view, View::Tips);
    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);

    app.on_key(Key::Char('i'));
    assert_eq!(app.ui.view, View::Skills);
    let _ = app.frame(100, 40);
    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);

    app.on_key(Key::Char('ш'));
    assert_eq!(app.ui.view, View::Skills);
    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);
}

#[test]
fn message_is_queued_for_the_session() {
    let sb = Sandbox::new("message-send");
    let mut app = start(&sb);
    app.on_key(Key::Enter);
    app.on_key(Key::Char('m'));
    assert!(app.ui.input.is_some());
    for c in "тест".chars() {
        app.on_key(Key::Char(c));
    }
    app.on_key(Key::Enter);

    let ask = read_json(&ask_file(&sb));
    let keys = ask["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].as_str().unwrap().starts_with("msg"));
    let texts = ask["texts"].as_array().unwrap();
    assert!(texts[0].as_str().unwrap().contains("тест"));
    assert!(flash(&app).starts_with("Сообщение отправлено"), "подвал: {}", flash(&app));
    assert!(app.ui.input.is_none());
}

#[test]
fn message_cancelled_by_esc_is_not_sent() {
    let sb = Sandbox::new("message-esc");
    let mut app = start(&sb);
    app.on_key(Key::Enter);
    app.on_key(Key::Char('m'));
    for c in "тест".chars() {
        app.on_key(Key::Char(c));
    }
    app.on_key(Key::Esc);

    assert!(!ask_file(&sb).exists(), "запроса быть не должно");
    assert_eq!(flash(&app), "Сообщение не отправлено.");
    assert!(app.ui.input.is_none());
}

#[test]
fn p_in_card_asks_for_plan() {
    let sb = Sandbox::new("ask-plan");
    let mut app = start(&sb);
    app.on_key(Key::Enter);
    app.on_key(Key::Char('p'));

    let ask = read_json(&ask_file(&sb));
    assert_eq!(ask["plan"], json!(true));
    assert_eq!(ask["autoPlan"], json!(false));
    assert_eq!(flash(&app), "План запрошен.");
}

#[test]
fn tip_script_sets_then_undoes_setting() {
    let sb = Sandbox::new("tip-script");
    let mut app = start(&sb);
    let settings = sb.root().join("settings.json");
    let bak = sb.root().join("settings.json.ccwatch-bak");
    let fix = cache_fix();

    // первый запуск: ставим значение
    let plan = app.m.settings.plan(&fix).expect("настройки ещё не заданы");
    assert!(!plan.undo);
    app.ui.set_confirm =
        Some(SetConfirm { id: SESSION_ID.into(), fix: fix.clone(), plan, until: clock::now() + 60000.0 });
    assert!(!app.on_key(Key::Char('y')));
    assert_eq!(read_json(&settings)["subagentPromptCacheTtl"], json!("1h"));
    assert!(bak.exists(), "копия settings.json.ccwatch-bak должна быть");
    assert!(flash(&app).starts_with("Готово:"), "подвал: {}", flash(&app));

    // повторный запуск: возвращаем как было, ключ удаляется
    let plan = app.m.settings.plan(&fix).expect("после правки скрипт должен вернуть прежнее");
    assert!(plan.undo);
    app.ui.set_confirm =
        Some(SetConfirm { id: SESSION_ID.into(), fix: fix.clone(), plan, until: clock::now() + 60000.0 });
    app.on_key(Key::Char('y'));
    assert!(read_json(&settings).get("subagentPromptCacheTtl").is_none());
    assert!(flash(&app).starts_with("Вернул как было:"), "подвал: {}", flash(&app));
}

#[test]
fn tip_script_cancelled_by_any_other_key() {
    let sb = Sandbox::new("tip-cancel");
    let mut app = start(&sb);
    let settings = sb.root().join("settings.json");
    let bak = sb.root().join("settings.json.ccwatch-bak");
    let fix = cache_fix();
    let before = fs::read(&settings).unwrap();

    for key in [Key::Char('n'), Key::Esc] {
        let plan = app.m.settings.plan(&fix).expect("настройки ещё не заданы");
        app.ui.set_confirm =
            Some(SetConfirm { id: SESSION_ID.into(), fix: fix.clone(), plan, until: clock::now() + 60000.0 });
        app.on_key(key);
        assert_eq!(flash(&app), "Настройка не изменена.");
        assert!(app.ui.set_confirm.is_none());
        assert_eq!(fs::read(&settings).unwrap(), before, "файл не должен меняться");
        assert!(!bak.exists(), "копии быть не должно");
    }
}

#[test]
fn node_hook_tip_comes_first_on_screen_and_has_no_price() {
    let sb = Sandbox::new("hook-tip-screen");
    let mut app = start(&sb);
    // хук ещё запускает Node: совет о хуке должен стоять первым и без цены
    let node = json!({"type": "command", "command": "cmd", "args": ["/d", "/c", "ccwatch-hook.cmd"], "async": true});
    let settings = json!({"hooks": {"PostToolUse": [{"hooks": [node]}]}, "modelSettings": {}});
    fs::write(sb.root().join("settings.json"), settings.to_string()).unwrap();
    app.m.requests.check_hook(&app.m.paths, clock::now() + 10_000.0);

    app.on_key(Key::Enter);
    app.on_key(Key::Char('s'));
    let frame = app.frame(100, 40);
    let body = frame.body.join("\n");
    let line = body.lines().find(|l| l.contains("Хук ccwatch работает через Node")).expect("совет о хуке на экране");
    assert!(!line.contains('≈'), "у совета о программе цены нет: {line}");
    assert!(frame.foot.contains("Enter выполнить скрипт"), "подвал: {}", frame.foot);

    app.on_key(Key::Esc);
    app.on_key(Key::Esc);
    app.on_key(Key::Enter);
    let card = app.frame(100, 40).body.join("\n");
    assert!(card.contains("первый — «Хук ccwatch работает через Node»"), "строка «Советы» в карточке");
}

#[test]
fn ideas_screen_asks_agent_and_goes_back() {
    let sb = Sandbox::new("ideas-screen");
    let mut app = start(&sb);
    app.on_key(Key::Enter);
    app.on_key(Key::Char('b'));
    assert_eq!(app.ui.view, View::Ideas);
    let empty = app.frame(100, 40).body.join("\n");
    assert!(empty.contains("Вопрос не задавали"), "до вопроса строка состояния: {empty}");

    // Enter спрашивает агента: в ask/ уходит служебный вопрос с меткой ccwatch:ideas
    app.on_key(Key::Enter);
    let ask = read_json(&ask_file(&sb));
    let keys = ask["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0], json!("ideas"));
    let texts = ask["texts"].as_array().unwrap();
    let text = texts[0].as_str().unwrap();
    assert!(text.contains("(ccwatch:ideas)"));
    // ответ агент пишет в файл этой сессии, а не в текст
    let file = sb.base().join("ideas").join(format!("{SESSION_ID}.md"));
    assert!(text.contains(&file.display().to_string()), "в вопросе путь к файлу: {text}");
    assert!(file.parent().unwrap().is_dir(), "папка ideas заведена при отправке");
    assert!(flash(&app).starts_with("Вопрос отправлен"), "подвал: {}", flash(&app));
    let waiting = app.frame(100, 40).body.join("\n");
    assert!(waiting.contains("дойдёт при следующем действии агента"), "экран: {waiting}");

    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);
    // русская раскладка: та же клавиша
    app.on_key(Key::Char('и'));
    assert_eq!(app.ui.view, View::Ideas);
    app.on_key(Key::Esc);
    assert_eq!(app.ui.view, View::Detail);
}

#[test]
fn ideas_show_on_card_and_screen() {
    let sb = Sandbox::new("ideas-show");
    let mut app = start(&sb);
    let i = app.sel_index().unwrap();
    app.m.sessions[i].ideas.push(ccwatch::ideas::Idea {
        t: clock::now(),
        name: "тесты без кэша".into(),
        detect: "tsc в каждом прогоне".into(),
        advice: "держать кэш компиляции".into(),
    });
    app.on_key(Key::Enter);
    let card = app.frame(100, 40);
    assert!(card.body.join("\n").contains("1 от агента"), "строка «Идеи» в карточке");
    assert!(card.foot.contains("b идеи"), "подвал: {}", card.foot);

    app.on_key(Key::Char('b'));
    let screen = app.frame(100, 40).body.join("\n");
    assert!(screen.contains("1. тесты без кэша"), "идея на экране: {screen}");
    assert!(screen.contains("Как заметить: tsc в каждом прогоне"), "экран: {screen}");
    assert!(screen.contains("Из других сессий · 0"), "экран: {screen}");
}
