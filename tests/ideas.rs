//! Копилка идей агента (~/.ccwatch/ideas.jsonl) и служебные ключи хука.
//! Данные — во временной папке теста (ccwatch-test-<pid>-<имя>); настоящие ~/.claude и ~/.ccwatch не трогаем.

use std::fs;
use std::path::PathBuf;

use ccwatch::ideas::{Idea, IdeaStore};
use ccwatch::paths::Paths;
use ccwatch::session::Session;
use ccwatch::tips::is_service_key;

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

    fn paths(&self) -> Paths {
        Paths::new(&self.dir.join("claude"), &self.dir.join("ccwatch"))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Сессия с идеями: у каждой одно название, время — как у строки ответа.
fn session(id: &str, ideas: &[(f64, &str)]) -> Session {
    let mut s = Session::new(id);
    for (t, name) in ideas {
        s.ideas.push(Idea {
            t: *t,
            name: name.to_string(),
            detect: "инструменты и длительности".into(),
            advice: "кэшировать".into(),
        });
    }
    s
}

fn lines(p: &Paths) -> usize {
    fs::read_to_string(&p.ideas).map(|t| t.lines().count()).unwrap_or(0)
}

#[test]
fn sync_writes_each_idea_once_across_reloads() {
    let sb = Sandbox::new("ideas-sync");
    let p = sb.paths();
    assert!(p.ideas.ends_with("ideas.jsonl"));
    let t1 = 1_760_000_000_000.0;
    let mut sessions = vec![
        session("s1", &[(t1, "первая"), (t1 + 100_000.0, "вторая")]),
        session("s2", &[(t1 + 200_000.0, "третья")]),
    ];

    let mut store = IdeaStore::load(&p);
    assert!(store.items.is_empty(), "файла ещё нет");
    store.sync(&p, &sessions);
    assert_eq!(store.items.len(), 3);
    assert_eq!(lines(&p), 3);

    // повторный разбор тех же идей ничего не дописывает
    store.sync(&p, &sessions);
    assert_eq!(lines(&p), 3);

    // перезапуск монитора: прежние идеи читаются, и sync их не дублирует
    let mut again = IdeaStore::load(&p);
    assert_eq!(again.items.len(), 3);
    again.sync(&p, &sessions);
    assert_eq!(lines(&p), 3);

    // новая идея дописывается одной строкой
    sessions[0].ideas.push(Idea {
        t: t1 + 300_000.0,
        name: "четвёртая".into(),
        detect: String::new(),
        advice: String::new(),
    });
    again.sync(&p, &sessions);
    assert_eq!(lines(&p), 4);
    assert_eq!(IdeaStore::load(&p).items.len(), 4);
}

#[test]
fn stored_line_has_session_project_and_text() {
    let sb = Sandbox::new("ideas-fields");
    let p = sb.paths();
    let t = 1_760_000_000_000.0;
    let sessions = vec![session("abc", &[(t, "тесты без кэша")])];
    let mut store = IdeaStore::load(&p);
    store.sync(&p, &sessions);

    let text = fs::read_to_string(&p.ideas).unwrap();
    let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(row["at"], serde_json::json!(1_760_000_000_000_i64));
    assert_eq!(row["session"], "abc");
    assert_eq!(row["name"], "тесты без кэша");
    assert_eq!(row["detect"], "инструменты и длительности");
    assert_eq!(row["advice"], "кэшировать");
    assert_eq!(store.items[0].project, "");
}

#[test]
fn load_skips_broken_lines() {
    let sb = Sandbox::new("ideas-broken");
    let p = sb.paths();
    fs::create_dir_all(&p.base).unwrap();
    let good = r#"{"at":1760000000000,"session":"s1","project":"demo","name":"идея","detect":"","advice":""}"#;
    fs::write(&p.ideas, format!("{good}\nне json\n{{}}\n\n")).unwrap();

    let store = IdeaStore::load(&p);
    assert_eq!(store.items.len(), 1);
    assert_eq!(store.items[0].project, "demo");
}

#[test]
fn ideas_and_messages_are_service_keys_not_tips() {
    assert!(is_service_key("ideas"));
    assert!(is_service_key("msg3k9x"));
    assert!(!is_service_key("repeat"));
    assert!(!is_service_key("ideasx"));
}
