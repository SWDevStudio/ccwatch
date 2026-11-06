//! Идеи агента для анализатора: что, по его опыту, отнимало время или токены и как это заметить по логам.
//! Идеи сессии приходят строками «ccwatch: идея …» в ответе агента (см. session/plan.rs). Все они копятся
//! в ~/.ccwatch/ideas.jsonl по всем сессиям: по строке JSON на идею, чтобы их можно было читать и превращать в правила.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::paths::{basename, Paths};
use crate::session::Session;

/// Идея из ответа агента, как её видит сессия. t — время строки в транскрипте, мс от 1970.
#[derive(Clone, Debug)]
pub struct Idea {
    pub t: f64,
    pub name: String,
    pub detect: String,
    pub advice: String,
}

/// Строка копилки: идея с сессией и папкой проекта. at — время ответа, мс от 1970.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredIdea {
    pub at: i64,
    pub session: String,
    pub project: String,
    pub name: String,
    pub detect: String,
    pub advice: String,
}

/// Копилка в памяти: что уже в файле, чтобы повторный разбор транскрипта и перезапуск не дублировали строки.
pub struct IdeaStore {
    pub items: Vec<StoredIdea>,
    /// ключи записанных идей: сессия, время, название
    known: HashSet<String>,
}

/// Ключ идеи: сессия, название, как заметить и что советовать. Время не входит: повторно записанный
/// файл с тем же содержимым (с новым mtime) не дублирует идеи в копилке.
fn key(session: &str, name: &str, detect: &str, advice: &str) -> String {
    format!("{session}\u{1}{name}\u{1}{detect}\u{1}{advice}")
}

impl IdeaStore {
    /// Читает копилку из файла при старте. Битые строки и отсутствующий файл пропускаем.
    pub fn load(paths: &Paths) -> IdeaStore {
        let mut store = IdeaStore { items: Vec::new(), known: HashSet::new() };
        let Ok(bytes) = fs::read(&paths.ideas) else { return store };
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Ok(item) = serde_json::from_str::<StoredIdea>(line) else { continue };
            store.known.insert(key(&item.session, &item.name, &item.detect, &item.advice));
            store.items.push(item);
        }
        store
    }

    /// Дописывает в файл идеи сессий, которых там ещё нет. Не вышло записать — попробуем в следующий раз.
    pub fn sync(&mut self, paths: &Paths, sessions: &[Session]) {
        let mut added: Vec<(String, StoredIdea)> = Vec::new();
        let mut batch = String::new();
        for s in sessions {
            let project = s.reg.as_ref().map(|r| basename(&r.cwd)).unwrap_or_default();
            for idea in &s.ideas {
                let at = idea.t as i64;
                let k = key(&s.id, &idea.name, &idea.detect, &idea.advice);
                if self.known.contains(&k) {
                    continue;
                }
                let item = StoredIdea {
                    at,
                    session: s.id.clone(),
                    project: project.clone(),
                    name: idea.name.clone(),
                    detect: idea.detect.clone(),
                    advice: idea.advice.clone(),
                };
                let Ok(line) = serde_json::to_string(&item) else { continue };
                batch.push_str(&line);
                batch.push('\n');
                added.push((k, item));
            }
        }
        if added.is_empty() {
            return;
        }
        let _ = fs::create_dir_all(&paths.base);
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.ideas)
            .and_then(|mut f| f.write_all(batch.as_bytes()));
        if written.is_err() {
            return;
        }
        for (k, item) in added {
            self.known.insert(k);
            self.items.push(item);
        }
    }
}
