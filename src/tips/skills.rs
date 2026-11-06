//! История скиллов: кто вызывал скилл (главный агент, субагент или вы через «/») и когда.

use indexmap::IndexMap;

use crate::session::Session;
use crate::text::clip;
use crate::transcript::By;

/// Встроенные команды Claude Code — не скиллы.
const BUILTIN: &[&str] = &[
    "add-dir",
    "agents",
    "background",
    "btw",
    "bug",
    "clear",
    "compact",
    "config",
    "context",
    "cost",
    "doctor",
    "effort",
    "exit",
    "export",
    "fast",
    "feedback",
    "help",
    "hooks",
    "ide",
    "init",
    "install-github-app",
    "login",
    "logout",
    "mcp",
    "memory",
    "model",
    "output-style",
    "permissions",
    "plugin",
    "plugins",
    "pr-comments",
    "privacy-settings",
    "rc",
    "release-notes",
    "remote-control",
    "resume",
    "review",
    "rewind",
    "sandbox",
    "status",
    "statusline",
    "tasks",
    "terminal-setup",
    "theme",
    "todos",
    "upgrade",
    "usage",
    "vim",
    "workflows",
];

/// Событие вызова скилла с указанием, кто его вызвал.
#[derive(Clone, Debug)]
pub struct SkillEvent {
    pub name: String,
    pub args: String,
    pub t: f64,
    pub by: By,
    /// Кто вызвал: «вы», «главный агент» или «агент «имя»».
    pub who: String,
}

/// Сколько раз вызывали скилл и кто именно (по подписи вызывающего).
#[derive(Clone, Debug, Default)]
pub struct SkillCount {
    pub n: usize,
    pub who: IndexMap<String, usize>,
}

#[derive(Clone, Debug, Default)]
pub struct SkillReport {
    /// Все вызовы, новые — первыми.
    pub events: Vec<SkillEvent>,
    pub by_skill: IndexMap<String, SkillCount>,
    /// Сколько раз скилл вызвал агент.
    pub agent: usize,
    /// Сколько раз вы вызвали скилл через «/» (не встроенную команду).
    pub user_skills: usize,
    /// Сколько раз вы вызвали встроенную команду.
    pub builtins: usize,
}

/// Встроенная ли команда (не скилл).
pub fn is_builtin(name: &str) -> bool {
    BUILTIN.contains(&name)
}

impl Session {
    /// История вызовов скиллов по транскриптам главного агента и субагентов.
    pub fn skill_use(&self) -> SkillReport {
        let mut events: Vec<SkillEvent> = Vec::new();
        if let Some(main) = &self.main {
            for e in &main.skills {
                let who = if e.by == By::User { "вы" } else { "главный агент" };
                events.push(skill_event(e, who.to_string()));
            }
        }
        for a in &self.agents {
            let who = format!("агент «{}»", clip(&a.name, 40));
            for e in &a.t.skills {
                events.push(skill_event(e, who.clone()));
            }
        }
        events.sort_by(|a, b| b.t.partial_cmp(&a.t).unwrap_or(std::cmp::Ordering::Equal));

        let mut by_skill: IndexMap<String, SkillCount> = IndexMap::new();
        for e in &events {
            if e.by == By::User && is_builtin(&e.name) {
                continue; // встроенные команды — не скиллы
            }
            let r = by_skill.entry(e.name.clone()).or_default();
            r.n += 1;
            *r.who.entry(e.who.clone()).or_insert(0) += 1;
        }
        let agent = events.iter().filter(|e| e.by == By::Agent).count();
        let user_skills = events.iter().filter(|e| e.by == By::User && !is_builtin(&e.name)).count();
        let builtins = events.iter().filter(|e| e.by == By::User && is_builtin(&e.name)).count();
        SkillReport { events, by_skill, agent, user_skills, builtins }
    }
}

fn skill_event(e: &crate::transcript::SkillUse, who: String) -> SkillEvent {
    SkillEvent { name: e.name.clone(), args: e.args.clone(), t: e.t, by: e.by, who }
}
