//! Хук `ccwatch hook`, определение подключённого хука и совет «Хук ccwatch…» с переключением settings.json.
//! Данные — во временной папке теста (ccwatch-test-<pid>-<имя>); настоящие ~/.claude и ~/.ccwatch не трогаем.

use std::fs;
use std::path::{Path, PathBuf};

use ccwatch::app::App;
use ccwatch::cli;
use ccwatch::clock;
use ccwatch::fixes::steps;
use ccwatch::hook;
use ccwatch::paths::Paths;
use ccwatch::requests::{HookKind, Requests};
use ccwatch::tips::Kind;
use serde_json::{json, Value};

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

    /// Пути монитора внутри песочницы: данные Claude Code и папка ccwatch.
    fn paths(&self) -> Paths {
        Paths::new(&self.dir.join("claude"), &self.dir.join("ccwatch"))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Записывает JSON-файл, создавая папки.
fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Читает JSON-файл.
fn read(file: &Path) -> Value {
    let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    serde_json::from_str(&text).unwrap()
}

/// Файл запроса сессии в ask/.
fn ask_file(paths: &Paths, id: &str) -> PathBuf {
    paths.ask.join(format!("{id}.json"))
}

/// Текущий путь к exe теста — так же, как его берёт монитор (без префикса \\?\).
fn exe_text() -> String {
    let s = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Хук старого вида: Node-скрипт через cmd (так ставил его совет).
fn node_hook() -> Value {
    json!({
        "type": "command",
        "command": "cmd",
        "args": ["/d", "/c", "C:\\work\\ccwatch\\ccwatch-hook.cmd"],
        "async": true
    })
}

/// Хук на ccwatch.exe.
fn exe_hook() -> Value {
    json!({"type": "command", "command": "C:\\ccwatch\\ccwatch.exe", "args": ["hook"], "async": true})
}

/// Монитор на временной папке: без сессий, с первым проходом (там уже проверен хук).
fn start(sb: &Sandbox) -> App {
    App::start(sb.paths(), cli::parse(&["--once".to_string()]), true)
}

/// Ответ хука на вход (session_id и т. п.); пусто — хук ничего не вернул.
fn process(paths: &Paths, input: &Value) -> Option<Value> {
    hook::process(paths, &input.to_string()).map(|out| serde_json::from_str(&out).unwrap())
}

/// Текст, который хук передал бы агенту.
fn context(out: &Value) -> String {
    out["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
}

#[test]
fn run_without_requests_returns_zero() {
    let sb = Sandbox::new("hook-empty");
    // папки ask нет совсем: хук выходит сразу, stdin не читает
    assert_eq!(hook::run(&sb.paths()), 0);
}

#[test]
fn plan_request_reaches_the_agent_once_and_is_marked_delivered() {
    let sb = Sandbox::new("hook-plan");
    let paths = sb.paths();
    let req = json!({
        "at": clock::now() as i64, "from": "ccwatch", "plan": true, "autoPlan": true,
        "keys": [], "texts": [], "ttl": 600_000
    });
    write(&ask_file(&paths, "sess-1"), &req);

    let out = hook::process(&paths, r#"{"session_id":"sess-1"}"#).expect("план должен уйти агенту");
    // формат ответа тот же, что у прежнего хука: тип события и additionalContext, порядок ключей сохранён
    assert!(out.starts_with(r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":""#), "{out}");
    let out: Value = serde_json::from_str(&out).unwrap();
    assert!(context(&out).contains("ccwatch: план"));

    assert!(!ask_file(&paths, "sess-1").exists(), "запрос удаляется после доставки");
    let done = read(&paths.done.join("sess-1.json"));
    assert_eq!(done["plan"], json!(true));
    assert_eq!(done["from"], json!("ccwatch"));
    assert!(done["deliveredAt"].is_i64(), "отметка о доставке");
    // повторный вызов ничего не отдаёт: запрос уже доставлен
    assert!(process(&paths, &json!({"session_id": "sess-1"})).is_none());
}

#[test]
fn advice_texts_are_joined_and_empty_ones_skipped() {
    let sb = Sandbox::new("hook-texts");
    let paths = sb.paths();
    let at = clock::now() as i64;
    // план не просили — только советы
    let texts = ["Совет один", "", "Совет два"];
    write(
        &ask_file(&paths, "a"),
        &json!({"at": at, "plan": false, "keys": ["k1", "k2"], "texts": texts, "ttl": 43_200_000}),
    );
    let out = process(&paths, &json!({"session_id": "a"})).expect("советы должны уйти");
    assert_eq!(context(&out), "Совет один\n\nСовет два");

    // старый запрос без поля plan, но с советами: это совет, а не просьба о плане
    write(&ask_file(&paths, "b"), &json!({"at": at, "texts": ["Только совет"]}));
    let out = process(&paths, &json!({"session_id": "b"})).expect("совет должен уйти");
    assert_eq!(context(&out), "Только совет");
}

#[test]
fn request_without_fields_is_a_plan_request() {
    let sb = Sandbox::new("hook-old");
    let paths = sb.paths();
    // самый старый формат: только время, без plan и texts
    write(&ask_file(&paths, "old"), &json!({"at": clock::now() as i64}));
    let out = process(&paths, &json!({"session_id": "old"})).expect("старый запрос — просьба о плане");
    assert!(context(&out).contains("ccwatch: план"));
}

#[test]
fn expired_request_gets_no_answer() {
    let sb = Sandbox::new("hook-expired");
    let paths = sb.paths();
    // автоматическая просьба десятиминутной давности: её уже никто не ждёт
    write(&ask_file(&paths, "sess-1"), &json!({"at": clock::now() as i64 - 11 * 60_000, "plan": true, "ttl": 600_000}));
    assert!(process(&paths, &json!({"session_id": "sess-1"})).is_none());
    assert!(!ask_file(&paths, "sess-1").exists(), "просроченный запрос удаляется");
    assert!(!paths.done.join("sess-1.json").exists(), "отметки о доставке нет");
}

#[test]
fn manual_request_waits_up_to_twelve_hours() {
    let sb = Sandbox::new("hook-manual");
    let paths = sb.paths();
    // отправленное вручную час назад ещё ждёт: срок 12 часов
    let hour_ago = clock::now() as i64 - 3_600_000;
    write(&ask_file(&paths, "sess-1"), &json!({"at": hour_ago, "plan": true, "ttl": 43_200_000}));
    assert!(process(&paths, &json!({"session_id": "sess-1"})).is_some());
}

#[test]
fn subagent_calls_are_skipped_and_request_is_kept() {
    let sb = Sandbox::new("hook-agent");
    let paths = sb.paths();
    write(&ask_file(&paths, "sess-1"), &json!({"at": clock::now() as i64, "plan": true, "ttl": 600_000}));
    assert!(process(&paths, &json!({"session_id": "sess-1", "agent_id": "agent-7"})).is_none());
    assert!(ask_file(&paths, "sess-1").exists(), "запрос ждёт вызова главного агента");
}

#[test]
fn session_id_is_cleaned_before_building_the_file_name() {
    let sb = Sandbox::new("hook-clean");
    let paths = sb.paths();
    write(&ask_file(&paths, "sess1"), &json!({"at": clock::now() as i64, "plan": true, "ttl": 600_000}));
    // точки и слеши из id убираются, как в прежнем хуке
    assert!(process(&paths, &json!({"session_id": "sess/../1"})).is_some());
    assert!(!ask_file(&paths, "sess1").exists());
}

#[test]
fn check_hook_tells_node_exe_and_no_hook_apart() {
    let sb = Sandbox::new("hook-kind");
    let paths = sb.paths();
    let now = 1e12;

    // файла нет
    let mut r = Requests::new();
    assert!(!r.check_hook(&paths, now));
    assert_eq!(r.hook_kind, HookKind::None);

    // старый Node-хук
    write(&paths.settings, &json!({"hooks": {"PostToolUse": [{"hooks": [node_hook()]}]}}));
    let mut r = Requests::new();
    assert!(r.check_hook(&paths, now));
    assert_eq!(r.hook_kind, HookKind::Node);

    // ccwatch.exe hook
    write(&paths.settings, &json!({"hooks": {"PostToolUse": [{"matcher": "", "hooks": [exe_hook()]}]}}));
    let mut r = Requests::new();
    assert!(r.check_hook(&paths, now));
    assert_eq!(r.hook_kind, HookKind::Exe);

    // Node важнее: если остался старый хук, совет предложит переключить его
    write(&paths.settings, &json!({"hooks": {"PostToolUse": [{"hooks": [exe_hook()]}, {"hooks": [node_hook()]}]}}));
    let mut r = Requests::new();
    r.check_hook(&paths, now);
    assert_eq!(r.hook_kind, HookKind::Node);

    // хук не в PostToolUse, а чужой хук — не ccwatch
    let other = json!({"type": "command", "command": "other.exe"});
    let settings = json!({"hooks": {"PreToolUse": [{"hooks": [exe_hook()]}], "PostToolUse": [{"hooks": [other]}]}});
    write(&paths.settings, &settings);
    let mut r = Requests::new();
    assert!(!r.check_hook(&paths, now));
    assert_eq!(r.hook_kind, HookKind::None);
}

#[test]
fn node_hook_tip_switches_settings_to_exe_and_undo_restores_it() {
    let sb = Sandbox::new("setup-switch");
    let paths = sb.paths();
    let other = json!({"type": "command", "command": "other-tool", "args": ["--x"]});
    let original = json!([
        {"matcher": "Bash", "hooks": [node_hook(), other.clone()]},
        {"hooks": [node_hook()]},
    ]);
    write(&paths.settings, &json!({"hooks": {"PostToolUse": original.clone()}, "modelSettings": {"x": 1}}));

    let app = start(&sb);
    let tips = app.m.global_tips();
    assert_eq!(tips.len(), 1, "совет о хуке один");
    let tip = &tips[0];
    assert_eq!(tip.key, "hook");
    assert_eq!(tip.kind, Kind::Setup);
    assert!(!tip.send, "агенту этот совет не отправляют");
    assert_eq!(tip.title, "Хук ccwatch работает через Node");
    let fix = tip.fix.clone().expect("у совета есть скрипт");

    // шаги показываем человеческим описанием
    let plan = app.m.settings.plan(&fix).expect("настройка ещё не стоит");
    assert!(!plan.undo);
    let lines = steps(&plan);
    assert!(
        lines.iter().any(|l| l.starts_with("2. в settings.json: хук PostToolUse: ccwatch-hook.cmd (Node) → ")),
        "{lines:?}"
    );
    let msg = app.m.settings.run(&fix, &plan);
    assert!(msg.starts_with("Готово:"), "{msg}");

    // чужой хук остался на месте, Node-хука нет, в конце группа ccwatch.exe hook; остальные ключи целы
    let after = read(&paths.settings);
    let post = after["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 2, "группа, ставшая пустой, убрана");
    assert_eq!(post[0], json!({"matcher": "Bash", "hooks": [other]}));
    assert_eq!(
        post[1],
        json!({"hooks": [{"type": "command", "command": exe_text(), "args": ["hook"], "async": true}]})
    );
    assert_eq!(after["modelSettings"], json!({"x": 1}));
    assert!(!fs::read_to_string(&paths.settings).unwrap().contains("ccwatch-hook"));
    assert!(paths.root.join("settings.json.ccwatch-bak").exists(), "копия до правки");
    assert!(app.m.settings.fix_done(&fix), "после выполнения совет помечен выполненным");

    // после переключения хук на ccwatch.exe, и совета о хуке больше нет
    let mut app = app;
    app.m.requests.check_hook(&app.m.paths, clock::now() + 10_000.0);
    assert!(app.m.global_tips().is_empty());

    // повторный запуск того же скрипта возвращает прежний массив
    let plan = app.m.settings.plan(&fix).expect("после правки скрипт вернёт прежнее");
    assert!(plan.undo);
    let msg = app.m.settings.run(&fix, &plan);
    assert!(msg.starts_with("Вернул как было:"), "{msg}");
    assert_eq!(read(&paths.settings)["hooks"]["PostToolUse"], original);
}

#[test]
fn missing_settings_file_gets_the_hook_without_backup() {
    let sb = Sandbox::new("setup-new-pc");
    let paths = sb.paths();
    // на новом ПК файла settings.json ещё нет, папки .claude тоже
    let app = start(&sb);
    let tips = app.m.global_tips();
    assert_eq!(tips.len(), 1);
    assert_eq!(tips[0].title, "Хук ccwatch не подключён");
    let fix = tips[0].fix.clone().expect("у совета есть скрипт");

    let plan = app.m.settings.plan(&fix).expect("файла нет — настройка не стоит");
    let msg = app.m.settings.run(&fix, &plan);
    assert!(msg.starts_with("Готово:"), "{msg}");
    assert!(!paths.root.join("settings.json.ccwatch-bak").exists(), "копировать было нечего");
    let after = read(&paths.settings);
    assert_eq!(
        after["hooks"]["PostToolUse"],
        json!([{"hooks": [{"type": "command", "command": exe_text(), "args": ["hook"], "async": true}]}])
    );
    assert!(app.m.settings.fix_done(&fix));
}
