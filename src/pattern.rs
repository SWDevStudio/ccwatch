//! Регулярные выражения записаны как в Node-версии — в синтаксисе JS, чтобы их легко было сверить.
//! js() переводит то, что в JS значит другое: \b, \w и \d там только про ASCII
//! (в Rust — про любые буквы и цифры), а `\/` в Rust не нужен.
//! Заглядывания вперёд и обратных ссылок в Rust нет — такие места разобраны вручную в коде.

use regex::Regex;

/// Шаблон JS с флагами ("i" — без учёта регистра).
pub fn js(src: &str, flags: &str) -> Regex {
    let mut out = String::new();
    if flags.contains('i') {
        out.push_str("(?i)");
    }
    let mut in_class = false;
    let mut chars = src.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let e = chars.next().unwrap_or('\\');
                match (e, in_class) {
                    ('b', false) => out.push_str(r"(?-u:\b)"),
                    ('w', false) => out.push_str("[0-9A-Za-z_]"),
                    ('w', true) => out.push_str("0-9A-Za-z_"),
                    ('d', false) => out.push_str("[0-9]"),
                    ('d', true) => out.push_str("0-9"),
                    ('/', _) => out.push('/'),
                    (e, _) => {
                        out.push('\\');
                        out.push(e);
                    }
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.push('[');
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            _ => out.push(c),
        }
    }
    Regex::new(&out).unwrap_or_else(|e| panic!("шаблон {src}: {e}"))
}

/// Шаблон, собранный один раз при первом использовании.
#[macro_export]
macro_rules! re {
    ($src:expr) => {
        $crate::re!($src, "")
    };
    ($src:expr, $flags:expr) => {{
        static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| $crate::pattern::js($src, $flags));
        &*RE
    }};
}
