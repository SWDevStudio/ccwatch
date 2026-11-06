//! Субагенты сессии: файлы <id>/subagents/*.jsonl и их <ключ>.meta.json — кто это, какой модели,
//! работает ли сейчас и сколько времени.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::Session;
use crate::clock;
use crate::json::{self, lenient};
use crate::transcript::Transcript;

/// Субагент: его имя, тип, модель, работает ли и его транскрипт.
pub struct Agent {
    pub name: String,
    /// agentType из meta.json
    pub kind: String,
    pub model: String,
    pub running: bool,
    pub t: Transcript,
}

/// meta.json субагента. Поля читаем мягко: кривой тип поля не должен ронять разбор.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Meta {
    #[serde(deserialize_with = "lenient")]
    pub description: String,
    #[serde(rename = "agentType", deserialize_with = "lenient")]
    pub agent_type: String,
    #[serde(deserialize_with = "lenient")]
    pub model: String,
    /// появляется только по завершении агента
    #[serde(deserialize_with = "lenient")]
    pub finished: Value,
}

/// Время работы субагента: от задания до последней реплики, у работающего — до сейчас.
/// Перерывы дольше получаса не в счёт (это делает active_at).
pub fn agent_time(a: &Agent, now: f64) -> f64 {
    let at = if a.running { now.max(a.t.last_at) } else { a.t.last_at };
    a.t.active_at(at)
}

fn read_meta(file: &Path) -> Option<Meta> {
    let text = fs::read_to_string(file).ok()?;
    serde_json::from_str(&text).ok()
}

impl Session {
    /// Обновляет список субагентов из папки <id>/subagents рядом с транскриптом сессии.
    pub(crate) fn pull_subagents(&mut self) {
        let Some(main_file) = self.main.as_ref().map(|m| m.file.clone()) else { return };
        let dir = main_file.parent().map(Path::to_path_buf).unwrap_or_default().join(&self.id).join("subagents");
        let Ok(entries) = fs::read_dir(&dir) else { return };
        let mut names: Vec<String> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();

        // транскрипты уже известных агентов переиспользуем: читаем с места прошлого чтения
        let mut known: HashMap<PathBuf, Transcript> =
            std::mem::take(&mut self.agents).into_iter().map(|a| (a.t.file.clone(), a.t)).collect();
        let mut list = Vec::new();
        for n in names {
            let Some(key) = n.strip_suffix(".jsonl").map(str::to_string) else { continue };
            let file = dir.join(&n);
            let mut t = known.remove(&file).unwrap_or_else(|| Transcript::new(file.clone()));
            t.pull(false, &mut |_| {});

            // meta перечитываем, пока в нём нет finished: до завершения агент может дописать его
            let finished_cached = self.metas.get(&key).is_some_and(|m| json::truthy(&m.finished));
            if !finished_cached {
                if let Some(m) = read_meta(&dir.join(format!("{key}.meta.json"))) {
                    self.metas.insert(key.clone(), m);
                }
            }
            let meta = self.metas.get(&key);
            let meta_desc = meta.map(|m| m.description.clone()).unwrap_or_default();
            let meta_type = meta.map(|m| m.agent_type.clone()).unwrap_or_default();
            let meta_model = meta.map(|m| m.model.clone()).unwrap_or_default();
            let finished = meta.is_some_and(|m| json::truthy(&m.finished));

            let name =
                [meta_desc, meta_type.clone()].into_iter().find(|s| !s.is_empty()).unwrap_or_else(|| key.clone());
            let model = if !t.model.is_empty() {
                t.model.clone()
            } else if !meta_model.is_empty() {
                meta_model
            } else {
                "?".to_string()
            };
            // без finished агент считаем работающим, пока его транскрипт менялся в последние полчаса
            let running = !finished && clock::now() - t.mtime < 30.0 * 60e3;
            list.push(Agent { name, kind: meta_type, model, running, t });
        }
        self.agents = list;
    }
}
