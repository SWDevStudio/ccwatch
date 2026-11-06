//! Ширина строк на экране: обрезка, выравнивание, перенос по словам.
//! Ширина — число символов без цветовых кодов (ESC [ … буква).

use crate::style;

/// Длина цветового кода, начинающегося в позиции i, или None.
fn csi_len(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&0x1b) || b.get(i + 1) != Some(&b'[') {
        return None;
    }
    let mut j = i + 2;
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';' || b[j] == b'?') {
        j += 1;
    }
    (j < b.len() && b[j].is_ascii_alphabetic()).then_some(j + 1 - i)
}

/// Видимая ширина: символы без цветовых кодов.
pub fn vis(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        if let Some(len) = csi_len(b, i) {
            i += len;
            continue;
        }
        // начало символа UTF-8 — всё, кроме байтов продолжения 10xxxxxx
        if b[i] & 0xC0 != 0x80 {
            n += 1;
        }
        i += 1;
    }
    n
}

/// Управляющие символы из чужого текста (табы, \f в путях) ломают отрисовку — заменяем пробелом.
/// ESC не трогаем: им начинаются цвета.
pub fn plain(s: &str) -> String {
    s.chars().map(|c| if (c < ' ' && c != '\x1b') || c == '\x7f' { ' ' } else { c }).collect()
}

/// Обрезает до ширины w, последним символом ставит «…».
pub fn clip(s: &str, w: i64) -> String {
    if w <= 0 {
        return String::new();
    }
    let a: Vec<char> = plain(s).chars().collect();
    if a.len() as i64 <= w {
        return a.into_iter().collect();
    }
    let mut out: String = a[..(w - 1) as usize].iter().collect();
    out.push('…');
    out
}

/// Обрезает и дополняет пробелами до ширины w.
pub fn pad(s: &str, w: i64) -> String {
    let c = clip(s, w);
    let fill = (w - vis(&c)).max(0) as usize;
    c + &" ".repeat(fill)
}

/// Левая часть у левого края, правая — у правого, между ними хотя бы пробел.
pub fn spread(left: &str, right: &str, w: i64) -> String {
    let gap = (w - vis(left) - vis(right)).max(1) as usize;
    format!("{left}{}{right}", " ".repeat(gap))
}

/// Выравнивание вправо, как padStart в JS.
pub fn pad_start(s: &str, w: usize) -> String {
    format!("{s:>w$}")
}

/// Обрезает уже раскрашенную строку по видимой ширине, чтобы она не переносилась.
pub fn clip_ansi(s: &str, w: i64) -> String {
    if vis(s) <= w {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::new();
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        if let Some(len) = csi_len(b, i) {
            out.push_str(&s[i..i + len]);
            i += len;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        if n >= w - 1 {
            out.push('…');
            break;
        }
        out.push(ch);
        n += 1;
        i += ch.len_utf8();
    }
    if style::color_on() {
        out.push_str("\x1b[0m");
    }
    out
}

/// Перенос по словам; слишком длинное слово режется.
pub fn wrap(text: &str, w: i64) -> Vec<String> {
    // в Node ширина всегда положительна; при нуле цикл ниже не закончился бы
    let w = w.max(1);
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in plain(text).split(' ').filter(|x| !x.is_empty()) {
        if cur.is_empty() {
            cur = word.to_string();
        } else if vis(&cur) + 1 + vis(word) <= w {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::replace(&mut cur, word.to_string()));
        }
        while vis(&cur) > w {
            let ch: Vec<char> = cur.chars().collect();
            out.push(ch[..w as usize].iter().collect());
            cur = ch[w as usize..].iter().collect();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Первые n символов (не байтов).
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
