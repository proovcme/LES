//! Versioned Unicode 15.1 NFKC/case folding, matching the former Python 3.13 authority.
use serde_json::{json, Value};
use std::{collections::HashMap, sync::OnceLock};
struct Tables {
    nfkd: HashMap<char, String>,
    fold: HashMap<char, String>,
    ccc: HashMap<char, u8>,
    compose: HashMap<(char, char), char>,
    decimal_zeroes: Vec<u32>,
}
fn scalar(value: &str) -> char {
    char::from_u32(u32::from_str_radix(value, 16).expect("compiled Unicode scalar"))
        .expect("compiled Unicode scalar")
}
fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let value: Value = serde_json::from_str(include_str!("../../config/unicode/15.1.0.json"))
            .expect("compiled Unicode data");
        let map = |key: &str| {
            value[key]
                .as_object()
                .expect("compiled Unicode map")
                .iter()
                .map(|(k, v)| {
                    (
                        scalar(k),
                        v.as_str().expect("compiled Unicode string").to_owned(),
                    )
                })
                .collect()
        };
        Tables {
            nfkd: map("nfkd"),
            fold: map("casefold"),
            decimal_zeroes: value["decimal_zeroes"]
                .as_array()
                .expect("compiled decimals")
                .iter()
                .map(|v| v.as_u64().unwrap() as u32)
                .collect(),
            ccc: value["combining"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (scalar(k), v.as_u64().unwrap() as u8))
                .collect(),
            compose: value["compose"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| {
                    let (a, b) = k.split_once('.').unwrap();
                    (
                        (scalar(a), scalar(b)),
                        char::from_u32(v.as_u64().unwrap() as u32).unwrap(),
                    )
                })
                .collect(),
        }
    })
}
fn combining(c: char) -> u8 {
    tables().ccc.get(&c).copied().unwrap_or(0)
}
fn pair(a: char, b: char) -> Option<char> {
    let (a, b) = (a as u32, b as u32);
    if (0x1100..0x1113).contains(&a) && (0x1161..0x1176).contains(&b) {
        return char::from_u32(0xAC00 + (a - 0x1100) * 588 + (b - 0x1161) * 28);
    }
    if (0xAC00..=0xD7A3).contains(&a) && (a - 0xAC00) % 28 == 0 && (0x11A8..0x11C3).contains(&b) {
        return char::from_u32(a + b - 0x11A7);
    }
    tables()
        .compose
        .get(&(char::from_u32(a)?, char::from_u32(b)?))
        .copied()
}
pub fn nfkc(text: &str) -> String {
    let mut decomposed = Vec::with_capacity(text.len());
    for c in text.chars() {
        let n = c as u32;
        if (0xAC00..=0xD7A3).contains(&n) {
            let s = n - 0xAC00;
            decomposed.push(char::from_u32(0x1100 + s / 588).unwrap());
            decomposed.push(char::from_u32(0x1161 + s % 588 / 28).unwrap());
            if s % 28 != 0 {
                decomposed.push(char::from_u32(0x11A7 + s % 28).unwrap());
            }
        } else if let Some(value) = tables().nfkd.get(&c) {
            decomposed.extend(value.chars());
        } else {
            decomposed.push(c);
        }
    }
    let mut start = 0;
    for index in 0..decomposed.len() {
        if combining(decomposed[index]) == 0 {
            decomposed[start..index].sort_by_key(|c| combining(*c));
            start = index + 1;
        }
    }
    decomposed[start..].sort_by_key(|c| combining(*c));
    let mut composed = Vec::<char>::with_capacity(decomposed.len());
    let mut starter = None;
    let mut last = 0;
    for c in decomposed {
        let class = combining(c);
        if let Some(index) = starter {
            if last < class || last == 0 {
                if let Some(value) = pair(composed[index], c) {
                    composed[index] = value;
                    continue;
                }
            }
        }
        if class == 0 {
            starter = Some(composed.len());
        }
        composed.push(c);
        last = class;
    }
    composed.into_iter().collect()
}
pub fn casefold(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for c in text.chars() {
        if let Some(value) = tables().fold.get(&c) {
            output.push_str(value);
        } else {
            output.push(c);
        }
    }
    output
}
pub fn lexical(text: &str) -> String {
    casefold(
        &nfkc(text)
            .chars()
            .map(|c| match c {
                '‐' | '‑' | '−' => '-',
                _ => c,
            })
            .collect::<String>(),
    )
    .replace('ё', "е")
}
/// Python-compatible decimal syntax; saturate only after all characters validate.
pub fn saturated_integer(text: &str) -> Option<i64> {
    let text = text.trim();
    let mut chars = text.chars().peekable();
    let negative = match chars.peek() {
        Some('-') => {
            chars.next();
            true
        }
        Some('+') => {
            chars.next();
            false
        }
        _ => false,
    };
    let mut magnitude = 0u64;
    let mut previous_digit = false;
    for c in chars {
        if c == '_' {
            if !previous_digit {
                return None;
            }
            previous_digit = false;
            continue;
        }
        let code = c as u32;
        let digit = if c.is_ascii_digit() {
            code - '0' as u32
        } else {
            tables()
                .decimal_zeroes
                .iter()
                .find_map(|base| code.checked_sub(*base).filter(|v| *v < 10))?
        };
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(u64::from(digit));
        previous_digit = true;
    }
    if !previous_digit {
        return None;
    }
    Some(if negative {
        if magnitude > i64::MAX as u64 {
            i64::MIN
        } else {
            -(magnitude as i64)
        }
    } else {
        magnitude.min(i64::MAX as u64) as i64
    })
}
pub fn execute(request: &Value) -> Result<Value, &'static str> {
    let mode = request["mode"].as_str().unwrap_or("lexical");
    if !matches!(mode, "nfkc" | "casefold" | "lexical") {
        return Err("UNICODE_MODE_INVALID");
    }
    let texts = request["texts"]
        .as_array()
        .filter(|v| v.len() <= 4096)
        .ok_or("UNICODE_INPUT_INVALID")?;
    let values = texts
        .iter()
        .map(|text| {
            let text = text.as_str().ok_or("UNICODE_INPUT_INVALID")?;
            Ok(match mode {
                "nfkc" => nfkc(text),
                "casefold" => casefold(text),
                _ => lexical(text),
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    Ok(json!({"version":"15.1.0","texts":values}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_composition_and_casefold_are_versioned() {
        assert_eq!(nfkc("\u{1100}\u{1161}\u{11A8}"), "각");
        assert_eq!(nfkc("e\u{301}"), "é");
        assert_eq!(
            lexical("ＳＰ № ２５６ ЁЛКА ‑ 3×2,5 Straße"),
            "sp No 256 елка - 3×2,5 strasse".to_lowercase()
        );
        assert_eq!(nfkc("\u{301}\u{323}"), "\u{323}\u{301}");
    }
}
