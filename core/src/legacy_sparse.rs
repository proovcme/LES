//! Persisted Qdrant TF vocabulary. Full BM25 uses the separate postings store.
use regex::Regex;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

const STOP_WORDS: &str = "какие какой какая какое каких каким какими где смотреть требования нормы норма требование найти пункт раздел свод правил гост сп случаях случае случай выполнять выполнение делать допускается допускать почему зачем что кто как когда куда откуда нужно должно следует необходимо быть может можно ли или для при под над все всех всеми чем тем только";

fn ordinary() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[0-9a-zа-яı.-]{2,}").expect("static sparse token pattern"))
}

fn technical() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"[A-ZА-ЯЁ0-9]+(?:[_./-][A-ZА-ЯЁ0-9]+)*").expect("static designation pattern")
    })
}

fn normalize(text: &str) -> String {
    // Project Python's full casefold onto the only characters this legacy
    // vocabulary can match. Spaces stand for discarded Unicode boundaries.
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            'A'..='Z' | 'А'..='Я' => out.extend(c.to_lowercase()),
            'a'..='z' | 'а'..='я' | '0'..='9' | 'ı' | '.' | '-' => out.push(c),
            'Ё' | 'ё' => out.push('е'),
            'K' => out.push('k'),
            'ſ' => out.push('s'),
            'İ' => out.push_str("i "),
            'ß' | 'ẞ' => out.push_str("ss"),
            'ŉ' => out.push_str(" n"),
            'ǰ' => out.push_str("j "),
            'ẖ' => out.push_str("h "),
            'ẗ' => out.push_str("t "),
            'ẘ' => out.push_str("w "),
            'ẙ' => out.push_str("y "),
            'ẚ' => out.push_str("a "),
            'ᲀ' => out.push('в'),
            'ᲁ' => out.push('д'),
            'ᲂ' => out.push('о'),
            'ᲃ' => out.push('с'),
            'ᲄ' | 'ᲅ' => out.push('т'),
            'ᲆ' => out.push('ъ'),
            'ﬀ' => out.push_str("ff"),
            'ﬁ' => out.push_str("fi"),
            'ﬂ' => out.push_str("fl"),
            'ﬃ' => out.push_str("ffi"),
            'ﬄ' => out.push_str("ffl"),
            'ﬅ' | 'ﬆ' => out.push_str("st"),
            _ => out.push(' '),
        }
    }
    out
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = i as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(value & 1));
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}

const CRC: [u32; 256] = crc_table();

fn term_id(term: &str) -> u32 {
    let crc = term.bytes().fold(u32::MAX, |crc, byte| {
        (crc >> 8) ^ CRC[((crc ^ u32::from(byte)) & 0xff) as usize]
    });
    !crc & 0x7fff_ffff
}

pub(crate) fn vector(text: &str) -> Option<Value> {
    static STOP: OnceLock<HashSet<&str>> = OnceLock::new();
    let stops = STOP.get_or_init(|| STOP_WORDS.split_whitespace().collect());
    let normalized = normalize(text);
    let mut ids = Vec::new();
    let mut counts = Vec::new();
    let mut positions = HashMap::new();
    let mut count = |term: &str| {
        let id = term_id(term);
        let position = *positions.entry(id).or_insert_with(|| {
            ids.push(id);
            counts.push(0u32);
            ids.len() - 1
        });
        counts[position] += 1;
    };
    let mut found = false;
    for item in ordinary().find_iter(&normalized) {
        let token = item.as_str().trim_matches(['.', '-']);
        if token.chars().count() >= 3 && !stops.contains(token) {
            count(crate::lexical::stem(token));
            found = true;
        }
    }
    if !found {
        for item in technical().find_iter(text) {
            // This alphabet contains only ASCII and basic Cyrillic. Separators
            // inside codes are kept verbatim by the Python fallback.
            let token = item.as_str().to_lowercase().replace('ё', "е");
            count(&token);
        }
    }
    (!ids.is_empty()).then(|| json!({"indices":ids,"values":counts}))
}

pub(super) fn execute(request: &Value) -> Result<Value, &'static str> {
    let text = request["text"].as_str().ok_or("LEGACY_SPARSE_INVALID")?;
    Ok(json!({"vector":vector(text)}))
}
