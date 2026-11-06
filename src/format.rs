//! Числа и длительности для экрана — с тем же округлением, что в Node-версии,
//! иначе «1.25k» и «1.3k» разошлись бы с прежним видом.

/// Math.round из JS: половина округляется вверх (−2.5 → −2).
pub fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// String(число) из JS: «523», «12.5», без «-0».
pub fn num(x: f64) -> String {
    if x == 0.0 {
        "0".into()
    } else {
        format!("{x}")
    }
}

/// toFixed из JS. Rust округляет ровную половину к чётному («1.25» → «1.2»), JS — вверх («1.3»):
/// берём точную десятичную запись двоичного числа и округляем по следующей цифре сами.
pub fn to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() {
        return num(x);
    }
    // 80 лишних знаков хватает, чтобы десятичная запись чисел крупнее 2⁻²⁸ была точной
    let exact = format!("{:.*}", digits + 80, x.abs());
    let (int, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = int.bytes().chain(frac.bytes().take(digits)).collect();
    if frac.as_bytes().get(digits).is_some_and(|&d| d >= b'5') {
        // +1 в последнем оставленном знаке, с переносом
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                break;
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    let int_len = kept.len() - digits;
    let mut s = String::from_utf8_lossy(&kept[..int_len]).into_owned();
    if digits > 0 {
        s.push('.');
        s.push_str(&String::from_utf8_lossy(&kept[int_len..]));
    }
    if x < 0.0 {
        s.insert(0, '-');
    }
    s
}

/// Токены: 523, 1.5k, 42k, 1.55M, 119.8M, 2.10B.
pub fn tok(n: f64) -> String {
    if n == 0.0 || n.is_nan() {
        return "0".into();
    }
    if n < 1e3 {
        return num(js_round(n));
    }
    if n < 1e6 {
        return to_fixed(n / 1e3, if n < 1e4 { 1 } else { 0 }) + "k";
    }
    if n < 1e9 {
        return to_fixed(n / 1e6, if n < 1e7 { 2 } else { 1 }) + "M";
    }
    to_fixed(n / 1e9, 2) + "B"
}

/// Сколько прошло: «42с», «5м 03с», «2ч 07м».
pub fn ago(ms: f64) -> String {
    let s = (ms / 1000.0).floor().max(0.0) as i64;
    if s < 60 {
        return format!("{s}с");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}м {:02}с", s % 60);
    }
    format!("{}ч {:02}м", m / 60, m % 60)
}

/// Длительность в минутах: «<1м», «15м», «1ч 30м».
pub fn dur(ms: f64) -> String {
    let m = js_round(ms / 60e3) as i64;
    if m < 1 {
        return "<1м".into();
    }
    if m < 60 {
        return format!("{m}м");
    }
    let h = m / 60;
    if m % 60 != 0 {
        format!("{h}ч {}м", m % 60)
    } else {
        format!("{h}ч")
    }
}

/// Оценка с тильдой: «~15м», но «<1м» без неё.
pub fn approx(ms: f64) -> String {
    let d = dur(ms);
    if d.starts_with('<') {
        d
    } else {
        format!("~{d}")
    }
}

pub fn pct(x: f64) -> String {
    format!("{}%", num(js_round(x * 100.0)))
}

pub fn mb(bytes: f64) -> String {
    format!("{} МБ", num(js_round(bytes / 1048576.0)))
}

/// Русское склонение: plural(3, "раз", "раза", "раз") → «раза».
pub fn plural<'a>(n: i64, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    let a = n.abs() % 100;
    let b = a % 10;
    if a > 10 && a < 20 {
        return many;
    }
    if b == 1 {
        return one;
    }
    if (2..=4).contains(&b) {
        few
    } else {
        many
    }
}

pub fn times(n: i64) -> String {
    format!("{n} {}", plural(n, "раз", "раза", "раз"))
}

/// Число в системе счисления 36, как `n.toString(36)` в JS (ключ сообщения).
pub fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_like_js() {
        assert_eq!(to_fixed(1.25, 1), "1.3");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1.005, 2), "1.00"); // в двоичном виде это чуть меньше 1.005
        assert_eq!(to_fixed(9.99, 1), "10.0");
        assert_eq!(tok(1250.0), "1.3k");
        assert_eq!(tok(686_000.0), "686k");
        assert_eq!(tok(119_800_000.0), "119.8M");
        assert_eq!(js_round(-25.5), -25.0);
        assert_eq!(base36(35), "z");
    }
}
