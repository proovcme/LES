//! Versioned lexical preparation; legacy defaults and index/query contracts stay explicit.
use regex::Regex;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

pub const LEGACY: &str = "les.lexical.v2";
pub const SNOWBALL: &str = "les.lexical.v3";

fn analyzed<'a>(word: &'a str, tokenizer: &str) -> Cow<'a, str> {
    if tokenizer == LEGACY {
        return Cow::Borrowed(stem(word));
    }
    // Identifiers and mixed scripts keep their existing exact token form.
    // Russian morphology applies only to words, never cable/model codes.
    if !word.chars().all(|c| ('а'..='я').contains(&c) || c == 'ё') {
        return Cow::Borrowed(word);
    }
    static RUSSIAN: OnceLock<qdrant_rust_stemmers::Stemmer> = OnceLock::new();
    RUSSIAN
        .get_or_init(|| {
            qdrant_rust_stemmers::Stemmer::create(qdrant_rust_stemmers::Algorithm::Russian)
        })
        .stem(word)
}

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
    let tokenizer = request["tokenizer"]
        .as_str()
        .ok_or("CORE_TOKENIZER_MISMATCH")?;
    if !matches!(tokenizer, LEGACY | SNOWBALL) {
        return Err("CORE_TOKENIZER_MISMATCH");
    }
    let raw = request.get("raw_texts").is_some();
    let normalized_texts = raw || request.get("texts").is_some();
    let words = request[if raw {
        "raw_texts"
    } else if normalized_texts {
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
        let normalized = if raw {
            Some(crate::unicode::lexical(
                row.as_str().ok_or("CORE_INVALID_DOCUMENTS")?,
            ))
        } else {
            None
        };
        let tokens: Vec<&str> = if normalized_texts {
            token_regex()
                .find_iter(
                    normalized
                        .as_deref()
                        .unwrap_or(row.as_str().ok_or("CORE_INVALID_DOCUMENTS")?),
                )
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
        let mut counts = BTreeMap::<String, u64>::new();
        let mut length = 0_u64;
        for word in tokens {
            if !stops.contains(word) {
                *counts
                    .entry(analyzed(word, tokenizer).into_owned())
                    .or_default() += 1;
                length += 1;
            }
        }
        documents.push(json!({"terms": counts, "length": length}));
    }
    Ok(json!({"documents": documents, "tokenizer": tokenizer}))
}

pub fn tokens(request: &Value) -> Result<Value, &'static str> {
    let action = request["action"]
        .as_str()
        .ok_or("CORE_TOKEN_ACTION_INVALID")?;
    if action == "stem" {
        return Ok(json!({"word":stem(request["word"].as_str().ok_or("CORE_INVALID_TOKEN")?)}));
    }
    if !matches!(action, "words" | "tokens" | "compact") {
        return Err("CORE_TOKEN_ACTION_INVALID");
    }
    let text = crate::unicode::lexical(request["text"].as_str().ok_or("CORE_INVALID_TOKEN")?);
    let stops: HashSet<&str> = request["stop_words"]
        .as_array()
        .ok_or("CORE_INVALID_STOP_WORDS")?
        .iter()
        .map(|v| v.as_str().ok_or("CORE_INVALID_STOP_WORDS"))
        .collect::<Result<_, _>>()?;
    let mut tokens = Vec::new();
    for matched in token_regex().find_iter(&text) {
        let word = matched.as_str();
        if action == "words" {
            tokens.push(word);
            continue;
        }
        if stops.contains(word) {
            continue;
        }
        if action == "compact" {
            if word.chars().count() < 3 || word.contains(['_', '/']) {
                tokens.push(word);
            }
        } else {
            tokens.push(stem(word));
        }
    }
    Ok(json!({"tokens":tokens}))
}

pub fn current(texts: &[String]) -> Result<Value, &'static str> {
    versioned(texts, LEGACY)
}

pub fn versioned(texts: &[String], tokenizer: &str) -> Result<Value, &'static str> {
    let config: Value = serde_json::from_str(include_str!("../../config/lexical-v2.json"))
        .map_err(|_| "CORE_LEXICAL_CONFIG_INVALID")?;
    prepare(&json!({"tokenizer":tokenizer,"raw_texts":texts,"stop_words":config["stop_words"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn russian_word_forms_share_terms_without_mutating_legacy_or_identifiers() {
        for words in [
            vec!["том", "тома", "томов"],
            vec!["заключение", "заключения"],
            vec!["ведомость", "ведомости"],
            vec!["оборудование", "оборудования"],
            vec!["рупорный", "рупорных"],
        ] {
            let texts = words.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            let result = versioned(&texts, SNOWBALL).unwrap();
            let first = &result["documents"][0]["terms"];
            assert!(!first.as_object().unwrap().is_empty());
            assert!(
                result["documents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|d| &d["terms"] == first),
                "{words:?}"
            );
        }
        let old = versioned(&["ведомости".into()], LEGACY).unwrap();
        assert_eq!(old["documents"][0]["terms"], json!({"ведомос":1}));
        // Snowball is not a morphological dictionary. Keep its known ambiguous
        // plural behavior visible instead of adding corpus-specific exceptions.
        let ambiguous = versioned(&["система".into(), "систем".into()], SNOWBALL).unwrap();
        assert_ne!(
            ambiguous["documents"][0]["terms"],
            ambiguous["documents"][1]["terms"]
        );
        let codes = vec!["ГР25.03 АСР-03.1.2 ПВ-3 TN-C-S 0.4 17,35".into()];
        assert_eq!(
            versioned(&codes, LEGACY).unwrap()["documents"],
            versioned(&codes, SNOWBALL).unwrap()["documents"]
        );
        assert_eq!(
            versioned(&codes, "unregistered").unwrap_err(),
            "CORE_TOKENIZER_MISMATCH"
        );
    }
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
