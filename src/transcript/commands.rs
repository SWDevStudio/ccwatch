//! Разбор команд оболочки: суть команды, имя программы, набор проверки и предикаты-шаблоны
//! (повтор, опрос, проверка, правка кода, заметка, проверяющий агент).

use regex::Regex;

use crate::json;
use crate::paths::basename;
use crate::re;

use super::entry::{collapse_spaces, tool_name, Block};

/// Делит команду по && ; и переводам строк, не заглядывая внутрь кавычек.
pub fn split_top(c: &str) -> Vec<String> {
    let cs: Vec<char> = c.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < cs.len() {
        let ch = cs[i];
        if let Some(q) = quote {
            cur.push(ch);
            // закрывающую кавычку, перед которой нет обратного слэша, — выходим из строки
            if ch == q && (i == 0 || cs[i - 1] != '\\') {
                quote = None;
            }
            i += 1;
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            cur.push(ch);
            i += 1;
            continue;
        }
        if ch == '\n' || ch == ';' || (ch == '&' && cs.get(i + 1) == Some(&'&')) {
            // && съедает оба символа
            i += if ch == '&' { 2 } else { 1 };
            parts.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(ch);
        i += 1;
    }
    parts.push(cur);
    parts.iter().map(|p| p.strip_suffix('\\').unwrap_or(p).trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Часть команды, которая что-то значит: без «cd …», «mkdir …», присваиваний переменных,
/// тела heredoc и длинных путей (остаётся имя файла).
pub fn cmd_core(c: &str) -> String {
    // heredoc: тело и хвост уходят, остаётся «<<ИМЯ …»; имя берём из той ветки, что сработала
    let body = match re!(r#"<<-?\s*(?:'(\w+)'|"(\w+)"|(\w+))[\s\S]*$"#).captures(c) {
        Some(m) => {
            let name = m.get(1).or(m.get(2)).or(m.get(3)).map_or("", |g| g.as_str());
            let start = m.get(0).map_or(c.len(), |g| g.start());
            format!("{}<<{} …", &c[..start], name)
        }
        None => c.to_string(),
    };
    // подоболочка «( … )» — по сути та же команда
    let parts: Vec<String> = split_top(&body)
        .into_iter()
        .map(|p| p.trim_start_matches('(').trim_start().trim_end_matches(')').trim().to_string())
        .collect();
    let keep: Vec<String> = parts.iter().filter(|p| !is_noise(p)).cloned().collect();
    let mut s = (if keep.is_empty() { parts } else { keep }).join(" && ");
    s = re!(r"^time\s+").replace(&s, "").into_owned();
    // длинный путь → «…/предпоследний/последний»
    s = re!(r#"(?:[A-Za-z]:)?(?:[\\/][^\s"'\\/]+){3,}"#)
        .replace_all(&s, |caps: &regex::Captures| {
            let segs: Vec<&str> = caps[0].split(|c| c == '/' || c == '\\').collect();
            format!("…/{}", segs[segs.len().saturating_sub(2)..].join("/"))
        })
        .into_owned();
    collapse_spaces(&s)
}

/// Служебная часть команды, которую не считаем сутью: cd, mkdir, присваивание переменной.
fn is_noise(p: &str) -> bool {
    re!(r"^(cd|mkdir|export|set|pushd|popd)\b").is_match(p) || re!(r#"^\w+=("[^"]*"|'[^']*'|\S*)$"#).is_match(p)
}

/// Токены команды без обёрток: time, env, nice, nohup, sudo, VAR=…, timeout N.
fn head_tokens(core: &str) -> Vec<&str> {
    let mut t: Vec<&str> = core.split(' ').filter(|s| !s.is_empty()).collect();
    while t.first().is_some_and(|f| is_wrapper(f) || re!(r"^\w+=").is_match(f)) {
        t.remove(0);
    }
    if t.first() == Some(&"timeout") {
        let skip = if t.get(1).is_some_and(|x| x.starts_with(|c: char| c.is_ascii_digit())) { 2 } else { 1 };
        let k = skip.min(t.len());
        t.drain(..k);
    }
    t
}

fn is_wrapper(s: &str) -> bool {
    matches!(s, "time" | "env" | "nice" | "nohup" | "sudo")
}

/// Последний кусок пути (после последнего / или \).
fn base(p: &str) -> &str {
    p.rsplit(|c: char| c == '/' || c == '\\').next().unwrap_or("")
}

/// Программы, которые лишь выполняют другой файл или код: важен файл, а не сама программа.
fn is_runner(prog: &str) -> bool {
    matches!(
        prog,
        "node"
            | "python"
            | "python3"
            | "py"
            | "bash"
            | "sh"
            | "npx"
            | "bun"
            | "deno"
            | "tsx"
            | "ts-node"
            | "pwsh"
            | "powershell"
    )
}

/// Программы, которые просмотр или пустая работа: повтором не считаем.
pub fn is_trivial(prog: &str) -> bool {
    matches!(
        prog,
        "ls" | "cat"
            | "grep"
            | "rg"
            | "find"
            | "head"
            | "tail"
            | "wc"
            | "echo"
            | "sed"
            | "awk"
            | "pwd"
            | "cp"
            | "mv"
            | "rm"
            | "touch"
            | "stat"
            | "du"
            | "tree"
            | "sort"
            | "uniq"
            | "diff"
            | "which"
            | "where"
            | "type"
            | "true"
            | "printf"
            | "sleep"
            | "test"
            | "["
            | "for"
            | "while"
            | "if"
            | "do"
            | "done"
            | "then"
            | "case"
            | "Get-ChildItem"
            | "Get-Content"
            | "Select-String"
    )
}

/// Чем считать повтором: та же программа или скрипт с любыми параметрами («node tools/sim/run.mjs»,
/// «npm run test»). Просмотр (ls, grep, git log…) и одноразовые скрипты (node -e, heredoc) — не повторы.
pub fn cmd_head(core: &str) -> String {
    let t = head_tokens(core);
    let prog = base(t.first().copied().unwrap_or(""));
    if prog.is_empty() || is_trivial(prog) {
        return String::new();
    }
    let args: Vec<&str> = t.iter().skip(1).filter(|x| !x.starts_with('-') && !x.starts_with("<<")).copied().collect();
    let a0 = args.first().copied().unwrap_or("");
    if prog == "git" {
        return if re!(r"^(status|log|diff|show|branch|rev-parse|ls-files)$").is_match(a0) {
            String::new()
        } else {
            format!("git {a0}")
        };
    }
    if matches!(prog, "npm" | "pnpm" | "yarn") {
        return std::iter::once(prog).chain(args.iter().take(2).copied()).collect::<Vec<_>>().join(" ");
    }
    if is_runner(prog) {
        let t1 = t.get(1).copied().unwrap_or("");
        return if re!(r"^-(e|c)$").is_match(t1) || t1 == "-" || args.is_empty() {
            String::new()
        } else {
            format!("{prog} {a0}")
        };
    }
    prog.to_string()
}

/// Имя программы в команде: без обёрток time / env / timeout N / VAR=…
pub fn prog_name(core: &str) -> String {
    base(head_tokens(core).first().copied().unwrap_or("")).to_string()
}

/// Ключ повтора вызова: одинаковый ключ — одна и та же работа (файл или команда с той же программой).
pub fn call_key(b: &Block) -> String {
    let i = &b.input;
    if b.name == "Read" {
        let fp = json::first_truthy(i, &["file_path"]);
        if !fp.is_empty() {
            return format!("read:{}", fp.replace('\\', "/").to_lowercase());
        }
    }
    if (b.name == "Bash" || b.name == "PowerShell") && json::has(i, "command") {
        let h = cmd_head(&cmd_core(&json::first_truthy(i, &["command"])));
        return if h.is_empty() { String::new() } else { format!("cmd:{h}") };
    }
    String::new()
}

/// Подпись вызова для строки «Сейчас» и списков.
pub fn call_label(b: &Block) -> String {
    let i = &b.input;
    if b.name == "Read" {
        let fp = json::first_truthy(i, &["file_path"]);
        if !fp.is_empty() {
            return basename(&fp);
        }
    }
    let cmd = json::first_truthy(i, &["command"]);
    if !cmd.is_empty() {
        return cmd_core(&cmd);
    }
    let d = json::first_truthy(i, &["description"]);
    tool_name(&b.name) + &if d.is_empty() { String::new() } else { format!(": {d}") }
}

/// Прогон проверки: тестовый набор, а не просмотр файла с таким именем.
fn check_re() -> &'static Regex {
    re!(
        r"\b(vitest|pytest|jest|mocha|checkpoint|suite)\b|\bplaywright\s+test\b|\b(cargo|go|dotnet)\s+test\b|\btest:\w+|\b(yarn|npm|pnpm|bun)(\s+run)?\s+test\b",
        "i"
    )
}

/// Команда — прогон проверки (тестовый набор).
pub fn is_check(core: &str) -> bool {
    check_re().is_match(core)
}

/// Набор проверки: команда с программы-прогонщика («vitest run …», «checkpoint.py run front.unit»),
/// без окружения и путей. Если прогонщика нет — вся суть команды.
pub fn suite_name(core: &str) -> String {
    let Some(m) = check_re().find(core) else { return core.to_string() };
    // lastIndexOf(' ', m.index) + 1: начало слова, в котором найден прогонщик
    let start = core.as_bytes()[..=m.start()].iter().rposition(|&b| b == b' ').map_or(0, |i| i + 1);
    let s = &core[start..];
    // .replace(/^\S*[\\/]/, ''): отбрасываем путь к файлу в первом слове
    let lead = s.find(char::is_whitespace).unwrap_or(s.len());
    match s[..lead].rfind(|c| c == '/' || c == '\\') {
        Some(i) => s[i + 1..].to_string(),
        None => s.to_string(),
    }
}

/// Команда по сути опрос: цикл с ожиданием или голый sleep (в PowerShell — с Start-Sleep).
pub fn is_poll(cmd: &str) -> bool {
    re!(r"\b(until|while|for|do)\b[\s\S]*\bsleep\b|^\s*(Start-)?sleep\s+(-\w+\s+)?\d", "i").is_match(cmd)
}

/// Команда, после которой код уже другой: git-операции, запись в файлы, скрипт из heredoc, установка зависимостей.
/// Часть про перенаправление «>» проверяется отдельно: см. redirects().
pub fn changes_code(cmd: &str) -> bool {
    re!(
        r"\bgit\s+(commit|merge|checkout|switch|reset|rebase|pull|cherry-pick|stash|apply|am|restore)\b|\bsed\s+-i|\btee\b|<<|\b(python3?|py|node)\s+-[ce]?\s|\b(cp|mv|rm|patch)\s|\b(yarn|npm|pnpm)\s+(install|add|ci)\b|\b(Set|Add)-Content\b|\bOut-File\b|\b(Copy|Move|Remove)-Item\b",
        "i"
    )
    .is_match(cmd)
        || redirects(cmd)
}

/// Перенаправление в файл: «>» в начале команды или после символа не из [0-9&>],
/// и за ним НЕ «&» и НЕ «пробелы + /dev/null». Заглядывания вперёд в regex нет — проверяем руками.
fn redirects(cmd: &str) -> bool {
    let cs: Vec<(usize, char)> = cmd.char_indices().collect();
    for (k, &(i, ch)) in cs.iter().enumerate() {
        if ch != '>' {
            continue;
        }
        let prev_ok = k == 0 || !matches!(cs[k - 1].1, '0'..='9' | '&' | '>');
        if !prev_ok {
            continue;
        }
        let rest = &cmd[i + 1..];
        if rest.starts_with('&') || rest.trim_start().starts_with("/dev/null") {
            continue;
        }
        return true;
    }
    false
}

/// Инструмент, который правит файлы.
pub fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// Заметки и черновики — не правка кода: .md, .txt, .log, Temp/tmp, scratchpad.
pub fn is_note_file(path: &str) -> bool {
    re!(r"\.(md|txt|log)$|[\\/](Temp|tmp)[\\/]|scratchpad", "i").is_match(path)
}

/// Проверяющий агент — по типу агента или по заданию.
pub fn is_check_role(s: &str) -> bool {
    re!(r"test|check|qa\b|review|verif|провер|прогон|ревью|аудит", "i").is_match(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_run_test_with_cd_is_repeat_key() {
        assert_eq!(cmd_head(&cmd_core("cd x && npm run test -- --watch")), "npm run test");
    }

    #[test]
    fn heredoc_body_is_dropped() {
        assert_eq!(cmd_core("python3 - <<'EOF'\nprint(1)\nEOF"), "python3 - <<EOF …");
        // у интерпретатора без файла, только с «-», повтора нет
        assert_eq!(cmd_head("python3 - <<EOF …"), "");
    }

    #[test]
    fn runner_keeps_script_name() {
        assert_eq!(cmd_head("node tools/sim/run.mjs --x"), "node tools/sim/run.mjs");
        assert_eq!(cmd_head("node -e 'console.log(1)'"), "");
    }

    #[test]
    fn timeout_and_env_are_wrappers() {
        assert_eq!(cmd_head("timeout 60 cargo test"), "cargo");
        // как в JS: обёртки снимаются до timeout, поэтому env остаётся программой
        assert_eq!(cmd_head("timeout 60 env FOO=1 cargo test"), "env");
        assert_eq!(prog_name("time nice cargo test"), "cargo");
    }

    #[test]
    fn redirect_rules() {
        assert!(changes_code("echo hi > out.txt"));
        assert!(!changes_code("cargo build 2>&1"));
        assert!(!changes_code("ls >/dev/null"));
        assert!(!changes_code("cmd 2>/dev/null"));
    }

    #[test]
    fn suite_is_tail_of_runner_command() {
        assert_eq!(suite_name("checkpoint.py run front.unit"), "checkpoint.py run front.unit");
        assert_eq!(suite_name("npx vitest run src/a.test.ts"), "vitest run src/a.test.ts");
    }
}
