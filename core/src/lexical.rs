//! Exact les.lexical.v2 stemming after Python-owned Unicode normalization.
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

pub(crate) fn token_regex() -> &'static Regex {
    static TOKENS: OnceLock<Regex> = OnceLock::new();
    // Python IGNORECASE additionally accepts dotless i. Other special case folds
    // have already passed through the shared Python NFKC/casefold authority.
    TOKENS.get_or_init(|| {
        Regex::new(r"[0-9a-zа-яёı]+(?:[._/-][0-9a-zа-яёı]+)*").expect("static lexical pattern")
    })
}

const ENDINGS: &[&str] = &[
    "иями",
    "ям",
    "ыми",
    "ейший",
    "ейшая",
    "ейшее",
    "ейшие",
    "ейших",
    "ого",
    "его",
    "ому",
    "ему",
    "ыми",
    "ими",
    "ых",
    "их",
    "ою",
    "ею",
    "ая",
    "яя",
    "ое",
    "ее",
    "ые",
    "ие",
    "ым",
    "им",
    "ом",
    "ем",
    "ах",
    "ях",
    "ов",
    "ев",
    "ей",
    "ам",
    "ям",
    "ит",
    "ет",
    "ут",
    "ют",
    "ат",
    "ят",
    "ти",
    "а",
    "ев",
    "ов",
    "е",
    "и",
    "й",
    "о",
    "у",
    "ы",
    "ь",
    "я",
    "ю",
    "ию",
];

pub fn stem(word: &str) -> &str {
    let length = word.chars().count();
    if length < 4 || !word.chars().all(|c| ('а'..='я').contains(&c) || c == 'ё') {
        return word;
    }
    for ending in ENDINGS {
        if let Some(prefix) = word.strip_suffix(ending) {
            if length - ending.chars().count() >= 4 {
                return prefix;
            }
        }
    }
    word
}

pub fn prepare(request: &Value) -> Result<Value, &'static str> {
    if request["tokenizer"] != "les.lexical.v2" {
        return Err("CORE_TOKENIZER_MISMATCH");
    }
    let normalized_texts = request.get("texts").is_some();
    let words = request[if normalized_texts {
        "texts"
    } else {
        "documents"
    }]
    .as_array()
    .ok_or("CORE_INVALID_DOCUMENTS")?;
    if words.len() > 256 {
        return Err("CORE_BATCH_LIMIT");
    }
    let stops: HashSet<&str> = request["stop_words"]
        .as_array()
        .ok_or("CORE_INVALID_STOP_WORDS")?
        .iter()
        .map(|v| v.as_str().ok_or("CORE_INVALID_STOP_WORDS"))
        .collect::<Result<_, _>>()?;
    if stops.len() > 512 {
        return Err("CORE_STOP_WORD_LIMIT");
    }
    let mut documents = Vec::with_capacity(words.len());
    let mut total = 0;
    for row in words {
        let tokens: Vec<&str> = if normalized_texts {
            token_regex()
                .find_iter(row.as_str().ok_or("CORE_INVALID_DOCUMENTS")?)
                .map(|item| item.as_str())
                .collect()
        } else {
            row.as_array()
                .ok_or("CORE_INVALID_DOCUMENTS")?
                .iter()
                .map(|value| value.as_str().ok_or("CORE_INVALID_TOKEN"))
                .collect::<Result<_, _>>()?
        };
        total += tokens.len();
        if total > 1_000_000 {
            return Err("CORE_TOKEN_LIMIT");
        }
        let mut counts = BTreeMap::<&str, u64>::new();
        let mut length = 0_u64;
        for word in tokens {
            if !stops.contains(word) {
                *counts.entry(stem(word)).or_default() += 1;
                length += 1;
            }
        }
        documents.push(json!({"terms": counts, "length": length}));
    }
    Ok(json!({"documents": documents, "tokenizer": "les.lexical.v2"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suffix_order_minimum_and_scripts_are_preserved() {
        assert_eq!(stem("оборудования"), "оборудовани");
        assert_eq!(stem("кабеля"), "кабел");
        assert_eq!(stem("земли"), "земл");
        assert_eq!(stem("щр-2"), "щр-2");
        assert_eq!(stem("сечения"), "сечени");
        assert_eq!(stem("PePE"), "PePE");
        assert_eq!(stem("пеpe"), "пеpe");
    }
    #[test]
    fn counts_stops_and_empty_documents() {
        let result = prepare(&json!({"tokenizer":"les.lexical.v2", "stop_words":["для"],
            "documents":[["кабеля", "кабель", "для", "pe"], []]}))
        .unwrap();
        assert_eq!(result["documents"][0]["terms"]["кабел"], 2);
        assert_eq!(result["documents"][0]["length"], 3);
        assert_eq!(result["documents"][1]["length"], 0);
    }
}
