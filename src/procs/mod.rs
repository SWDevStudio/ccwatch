//! Где запущена сессия и сколько памяти у её процессов.
//! Идём от процесса claude вверх по родителям: оболочка над ним — это вкладка терминала,
//! первое знакомое приложение выше — где открыта вкладка, его заголовок — окно.
//! Таблицу процессов берём синхронно: Windows API на Windows, `ps` на macOS и Linux.
//! В Node-версии PowerShell стартовал раз в 30 секунд и съедал память, здесь этого нет.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::clock;
use crate::format;
use crate::pattern;
use crate::registry::Registration;
use crate::session::Session;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix::{kill_tree, table};
#[cfg(windows)]
use windows::{kill_tree, table};

/// Строка таблицы процессов: родитель, имя exe, рабочий набор памяти в байтах,
/// время создания (мс от 1970; 0 — неизвестно) и заголовок главного окна.
#[derive(Clone, Debug, Default)]
pub struct Proc {
    pub ppid: u32,
    pub name: String,
    pub mem: f64,
    pub started: f64,
    pub title: String,
}

/// Оболочка, в которой запущен claude: название и когда её открыли.
#[derive(Clone, Debug)]
pub struct Shell {
    pub name: &'static str,
    pub started: f64,
}

/// Результат разбора цепочки: приложение, заголовок его окна и оболочка.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub app: String,
    pub title: String,
    pub shell: Option<Shell>,
}

/// Где запущена сессия и память её дерева процессов.
#[derive(Clone, Debug)]
pub struct Place {
    pub info: Info,
    pub mem: f64,
    pub procs: usize,
}

/// Кэш «где запущена» по pid сессий; обновляется по запросу, а не в фоне.
pub struct Procs {
    /// Когда таблица обновлялась последний раз (мс от 1970).
    pub at: f64,
    places: HashMap<u32, Place>,
}

/// Приложения, в которых может быть открыта вкладка терминала.
/// Название None — берём имя процесса без `.exe`.
static APPS: LazyLock<Vec<(Regex, Option<&'static str>)>> = LazyLock::new(|| {
    vec![
        (pattern::js(r"^code(\.exe)?$", "i"), Some("VS Code")),
        (pattern::js(r"^cursor(\.exe)?$", "i"), Some("Cursor")),
        (pattern::js(r"^windsurf(\.exe)?$", "i"), Some("Windsurf")),
        (pattern::js(r"^windowsterminal(\.exe)?$", "i"), Some("Windows Terminal")),
        (pattern::js(r"^claude(\.exe)?$", "i"), Some("Claude (десктоп)")),
        (
            pattern::js(r"^(idea|webstorm|pycharm|rider|goland|clion|phpstorm|rubymine)(64)?(\.exe)?$", "i"),
            Some("JetBrains IDE"),
        ),
        (
            pattern::js(
                r"^(iterm2|terminal|alacritty|kitty|wezterm|wezterm-gui|ghostty|warp|tabby|hyper|konsole|gnome-terminal-server|tmux)(\.exe)?$",
                "i",
            ),
            None,
        ),
    ]
});

/// Оболочки: по ним видно, в каком терминале открыта сессия.
static SHELLS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (pattern::js(r"^bash(\.exe)?$", "i"), "bash"),
        (pattern::js(r"^pwsh(\.exe)?$", "i"), "PowerShell 7"),
        (pattern::js(r"^powershell(\.exe)?$", "i"), "Windows PowerShell"),
        (pattern::js(r"^cmd(\.exe)?$", "i"), "cmd"),
        (pattern::js(r"^zsh$", ""), "zsh"),
        (pattern::js(r"^fish$", ""), "fish"),
        (pattern::js(r"^sh$", ""), "sh"),
        (pattern::js(r"^wsl(\.exe)?$", "i"), "WSL"),
    ]
});

impl Default for Procs {
    fn default() -> Self {
        Self::new()
    }
}

impl Procs {
    pub fn new() -> Procs {
        Procs { at: 0.0, places: HashMap::new() }
    }

    /// Обновляет место и память для pid сессий. Один снимок таблицы на все pid.
    pub fn refresh(&mut self, pids: &[u32]) {
        let list: Vec<u32> = pids.iter().copied().filter(|&p| p != 0).collect();
        if list.is_empty() {
            return;
        }
        let table = table();
        let kids = children(&table);
        for pid in list {
            let place = place_of(&table, &kids, pid);
            self.places.insert(pid, place);
        }
        self.at = clock::now();
    }

    /// Есть ли для pid уже определённое место (иначе в карточке «ищу окно…»).
    pub fn knows(&self, pid: u32) -> bool {
        self.places.contains_key(&pid)
    }

    pub fn get(&self, pid: u32) -> Option<&Place> {
        self.places.get(&pid)
    }

    /// Строка «Запущена: …» для карточки сессии.
    pub fn where_line(&self, s: &Session) -> String {
        let default = Registration::default();
        let r = s.reg.as_ref().unwrap_or(&default);
        let since =
            if r.started_at != 0.0 { format!("сессия с {}", clock::clock(r.started_at)) } else { String::new() };
        let w = self.get(r.pid);
        if r.entrypoint == "claude-desktop" {
            return join(&["приложение Claude (десктоп), вкладка Code", &since, &mem_text(w)]);
        }
        if !s.live {
            return since;
        }
        let Some(w) = w else {
            return join(&["ищу окно…", &since]);
        };
        let info = &w.info;
        if info.app.is_empty() && info.shell.is_none() {
            return join(&["окно определить не удалось", &since, &mem_text(Some(w))]);
        }
        let mut app_text = info.app.clone();
        if !info.title.is_empty() && !info.title.starts_with(&info.app) {
            app_text += &format!(" «{}»", strip_editor(&info.title));
        }
        let term_text = match &info.shell {
            Some(sh) => {
                let started = if sh.started != 0.0 {
                    format!(" (открыт {})", clock::clock(sh.started))
                } else {
                    String::new()
                };
                format!("терминал {}{}", sh.name, started)
            }
            None => String::new(),
        };
        join(&[&app_text, &term_text, &since, &mem_text(Some(w))])
    }
}

/// Строка памяти процесса: «процесс 120 МБ (с 3 дочерними)»; пусто, если памяти нет.
pub fn mem_text(p: Option<&Place>) -> String {
    match p {
        Some(w) if w.mem != 0.0 => {
            let mut s = format!("процесс {}", format::mb(w.mem));
            if w.procs > 1 {
                s += &format!(" (с {} дочерними)", w.procs - 1);
            }
            s
        }
        _ => String::new(),
    }
}

/// Завершить CLI-сессию вместе с дочерними процессами. Транскрипт остаётся, сессию можно вернуть.
/// Десктопные не трогаем: их процессами управляет приложение.
pub fn kill_session(s: &Session, done: Box<dyn FnOnce(String) + Send>) {
    let default = Registration::default();
    let r = s.reg.as_ref().unwrap_or(&default);
    if r.entrypoint == "claude-desktop" {
        return done("Десктопную сессию закройте в самом приложении (в архив): иначе оно перезапустит процесс или покажет ошибку.".into());
    }
    if !s.live || r.pid == 0 {
        return done("Сессия уже не запущена.".into());
    }
    let ok = format!("Сессия {} завершена. Вернуть: claude --resume {} в папке {}", s.name(), s.id, r.cwd);
    kill_tree(r.pid, ok, done);
}

/// Разбирает цепочку родителей (сам claude первым) и находит приложение, окно и оболочку.
/// Оболочка над claude — вкладка терминала; первое знакомое приложение выше — где она открыта.
pub fn describe_chain(chain: &[&Proc]) -> Info {
    let mut shell: Option<Shell> = None;
    let mut app = String::new();
    let mut title = String::new();
    let up: &[&Proc] = chain.get(1..).unwrap_or(&[]); // [0] — сам claude
    for i in 0..up.len() {
        let p = up[i];
        if let Some((_, name)) = SHELLS.iter().find(|(re, _)| re.is_match(&p.name)) {
            // самая внешняя оболочка — вкладка: более внешние перезаписывают внутренние
            shell = Some(Shell { name, started: p.started });
        }
        let ap = APPS.iter().find(|(re, _)| re.is_match(&p.name));
        if !p.title.is_empty() && title.is_empty() {
            title = p.title.clone();
        }
        let Some((_, label)) = ap else { continue };
        app = match label {
            Some(l) => l.to_string(),
            None => strip_exe(&p.name),
        };
        // у VS Code и похожих вкладкой владеет дочерний процесс, а заголовок окна — у главного над ним
        for later in &up[i..] {
            if !title.is_empty() || later.name != p.name {
                break;
            }
            title = later.title.clone();
        }
        break;
    }
    if app.is_empty() && shell.is_some() {
        app = "отдельное окно консоли".to_string();
    }
    Info { app, title, shell }
}

/// Цепочка родителей от процесса pid, не больше 12 уровней; первым идёт сам процесс.
fn chain_of(table: &HashMap<u32, Proc>, pid: u32) -> Vec<&Proc> {
    let mut chain = Vec::new();
    let mut cur = table.get(&pid);
    while let Some(p) = cur {
        if chain.len() >= 12 {
            break;
        }
        chain.push(p);
        cur = table.get(&p.ppid);
    }
    chain
}

/// Дочерние процессы по родителям. Процесс, который сам себе родитель, в детях не числится.
fn children(table: &HashMap<u32, Proc>) -> HashMap<u32, Vec<u32>> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&pid, p) in table {
        if p.ppid != pid {
            kids.entry(p.ppid).or_default().push(pid);
        }
    }
    kids
}

/// Память и число процессов всего дерева с корнем root (сам root тоже учитывается).
fn tree_totals(table: &HashMap<u32, Proc>, kids: &HashMap<u32, Vec<u32>>, root: u32) -> (f64, usize) {
    let mut mem = 0.0;
    let mut n = 0;
    let mut seen = HashSet::new();
    let mut stack = vec![root];
    while let Some(x) = stack.pop() {
        if !seen.insert(x) {
            continue;
        }
        if let Some(p) = table.get(&x) {
            mem += p.mem;
            n += 1;
        }
        if let Some(c) = kids.get(&x) {
            stack.extend(c.iter().copied());
        }
    }
    (mem, n)
}

fn place_of(table: &HashMap<u32, Proc>, kids: &HashMap<u32, Vec<u32>>, pid: u32) -> Place {
    let chain = chain_of(table, pid);
    let (mem, procs) = tree_totals(table, kids, pid);
    Place { info: describe_chain(&chain), mem, procs }
}

/// «Code.exe» → «Code»: имя приложения без хвоста `.exe`, регистр не важен.
fn strip_exe(name: &str) -> String {
    let n = name.len();
    match name.get(n.saturating_sub(4)..) {
        Some(tail) if n >= 4 && tail.eq_ignore_ascii_case(".exe") => name[..n - 4].to_string(),
        _ => name.to_string(),
    }
}

/// Убирает из заголовка окна редактора хвост « - Visual Studio Code» и подобные.
fn strip_editor(title: &str) -> &str {
    for suffix in [" - Visual Studio Code", " - Cursor", " - Windsurf"] {
        if let Some(t) = title.strip_suffix(suffix) {
            return t;
        }
    }
    title
}

/// Склеивает непустые части через «·».
fn join(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}
