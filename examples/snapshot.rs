//! Сверка кадров с эталоном Node-версии: для каждого сценария из scenarios.json строит кадр так же,
//! как golden-node.js, и пишет его в <папка вывода>/<имя>.txt.
//! Использование: cargo run --example snapshot -- <scenarios.json> <папка fixture> <папка вывода> [имена…]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use ccwatch::app::App;
use ccwatch::cli;
use ccwatch::clock;
use ccwatch::paths::Paths;
use ccwatch::style;
use ccwatch::ui::{Input, KillConfirm, SetConfirm};

/// Файл сценариев: общее «сейчас» и список.
#[derive(Deserialize)]
struct Scenarios {
    now: f64,
    scenarios: Vec<Scenario>,
}

/// Один сценарий: аргументы, цвет, размер и состояние интерфейса (как в golden-node.js).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Scenario {
    name: String,
    args: Vec<String>,
    color: Option<bool>,
    size: Option<[i64; 2]>,
    state: Option<State>,
}

/// Состояние интерфейса, которое ставит сценарий перед кадром.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct State {
    flash: Option<String>,
    tip: Option<String>,
    tip_sel: Option<i64>,
    confirm: Option<String>,
    plan_scroll: Option<i64>,
    sk_scroll: Option<i64>,
    kill: Option<bool>,
    input: Option<String>,
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("использование: snapshot <scenarios.json> <папка fixture> <папка вывода> [имена…]");
        std::process::exit(2);
    }
    let text = fs::read_to_string(&args[0]).expect("не прочитан scenarios.json");
    let file: Scenarios = serde_json::from_str(&text).expect("scenarios.json не разобран");
    let fixture = PathBuf::from(&args[1]);
    let out_dir = PathBuf::from(&args[2]);
    let only = &args[3..];
    fs::create_dir_all(&out_dir).expect("не создана папка вывода");

    for sc in &file.scenarios {
        if !only.is_empty() && !only.contains(&sc.name) {
            continue;
        }
        let out = run_scenario(sc, file.now, &fixture);
        fs::write(out_dir.join(format!("{}.txt", sc.name)), format!("{out}\n")).expect("не записан кадр");
        println!("{:<20} {} строк", sc.name, out.split('\n').count());
    }
}

/// Строит кадр сценария: без размера и состояния — снимок --once, иначе кадр с выставленным состоянием.
fn run_scenario(sc: &Scenario, now: f64, fixture: &Path) -> String {
    let (w, h) = match sc.size {
        Some([w, h]) => (w, h),
        None => (100, 200),
    };
    style::set_color(sc.color != Some(false));
    clock::freeze(now);

    let paths = Paths::new(&fixture.join(".claude"), &fixture.join(".ccwatch"));
    let mut argv = vec!["--once".to_string()];
    argv.extend(sc.args.iter().cloned());
    let mut app = App::start(paths, cli::parse(&argv), true);
    // снимок --once обновляет окна и память, как прогон Node с --once; его же печатает эталон
    let once = app.snapshot(w);
    if sc.size.is_none() && sc.state.is_none() {
        app.quit();
        return once;
    }
    let state = sc.state.as_ref().unwrap_or(&EMPTY_STATE);
    apply_state(&mut app, state, now);
    let screen = app.screen(w, h).join("\n");
    app.quit();
    screen
}

/// Пустое состояние для сценариев, где его нет.
static EMPTY_STATE: State = State {
    flash: None,
    tip: None,
    tip_sel: None,
    confirm: None,
    plan_scroll: None,
    sk_scroll: None,
    kill: None,
    input: None,
};

/// Номер выбранной сессии; у сценариев состояния она всегда есть.
fn selected(app: &App) -> usize {
    app.sel_index().expect("в сценарии нет выбранной сессии")
}

/// Номер совета с ключом в списке советов сессии, как findIndex в Node (-1 — нет).
fn tip_index(app: &App, i: usize, now: f64, key: &str) -> i64 {
    app.m.sessions[i].active_tips(now).active.iter().position(|t| t.key == key).map_or(-1, |p| p as i64)
}

/// Выставляет состояние интерфейса в том же порядке, что golden-node.js.
fn apply_state(app: &mut App, st: &State, now: f64) {
    if let Some(text) = &st.flash {
        app.ui.flash(text.as_str(), 5000.0);
    }
    if let Some(key) = &st.tip {
        let i = selected(app);
        app.ui.tip_sel = tip_index(app, i, now, key);
    }
    if let Some(v) = st.tip_sel {
        app.ui.tip_sel = v;
    }
    if let Some(key) = &st.confirm {
        let i = selected(app);
        app.ui.tip_sel = tip_index(app, i, now, key);
        let tips = app.m.sessions[i].active_tips(now).active;
        let tip = tips.get(app.ui.tip_sel as usize).expect("совет для подтверждения");
        let fix = tip.fix.clone().expect("у совета нет скрипта");
        let plan = app.m.settings.plan(&fix).expect("скрипт уже выполнен");
        let id = app.m.sessions[i].id.clone();
        app.ui.set_confirm = Some(SetConfirm { id, fix, plan, until: now + 60000.0 });
    }
    if let Some(v) = st.plan_scroll {
        app.ui.plan_scroll = Some(v);
    }
    if let Some(v) = st.sk_scroll {
        app.ui.sk_scroll = v;
    }
    if st.kill == Some(true) {
        let i = selected(app);
        let id = app.m.sessions[i].id.clone();
        app.ui.confirm = Some(KillConfirm { id, until: now + 15000.0 });
    }
    if let Some(text) = &st.input {
        let i = selected(app);
        let id = app.m.sessions[i].id.clone();
        app.ui.input = Some(Input { id, text: text.clone() });
    }
}
