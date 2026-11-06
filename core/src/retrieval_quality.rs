//! Lexical retrieval diagnostics and the existing wider-search policy.
use crate::lexical::{stem, token_regex};
use serde_json::{json, Value};
use std::collections::HashSet;

fn text<'a>(request: &'a Value, key: &str) -> Result<&'a str, &'static str> {
    request[key]
        .as_str()
        .ok_or("CORE_RETRIEVAL_QUALITY_INVALID")
}

fn words(value: &str) -> impl Iterator<Item = &str> {
    token_regex().find_iter(value).map(|word| word.as_str())
}

pub fn evaluate(request: &Value) -> Result<Value, &'static str> {
    let stops: HashSet<_> = request["stop_words"]
        .as_array()
        .filter(|items| items.len() <= 512)
        .ok_or("CORE_INVALID_STOP_WORDS")?
        .iter()
        .map(|word| word.as_str().ok_or("CORE_INVALID_STOP_WORDS"))
        .collect::<Result<_, _>>()?;
    let terms: HashSet<_> = words(text(request, "question")?)
        .filter(|word| !stops.contains(word))
        .map(stem)
        .collect();
    let mut matched = HashSet::new();
    for term in words(text(request, "content")?)
        .filter(|word| !stops.contains(word))
        .map(stem)
    {
        if let Some(term) = terms.get(term) {
            matched.insert(*term);
        }
    }
    let coverage = |count| {
        if terms.is_empty() {
            1.0
        } else {
            count as f64 / terms.len() as f64
        }
    };
    let body_coverage = if terms.is_empty() {
        0.0
    } else {
        coverage(matched.len())
    };
    for term in words(text(request, "names")?)
        .filter(|word| !stops.contains(word))
        .map(stem)
    {
        if let Some(term) = terms.get(term) {
            matched.insert(*term);
        }
    }
    let search_coverage = coverage(matched.len());
    let diversity = request["source_diversity"]
        .as_u64()
        .ok_or("CORE_RETRIEVAL_QUALITY_INVALID")?;
    let score = request["top_score"]
        .as_f64()
        .ok_or("CORE_RETRIEVAL_QUALITY_INVALID")?;
    let hybrid = request["hybrid"] == true;
    let retry_reason =
        if request["dense_similarity"] == true && score < 0.42 && search_coverage < 0.34 {
            Some("low_dense_score_and_term_coverage")
        } else if search_coverage < 0.25 && diversity > 3 {
            Some("broad_low_coverage")
        } else if search_coverage == 0.0 {
            Some("no_lexical_support")
        } else if hybrid && search_coverage < 0.25 && request["exact_refs"] != true {
            Some("hybrid_partial_support")
        } else {
            None
        };
    let (status, detail, retry_wider) = if request["embedding_mismatch"] == true {
        (
            "degraded",
            "lexical_only_embedding_contract_mismatch",
            false,
        )
    } else if request["ambiguous"] == true && diversity > 2 {
        ("needs_clarification", "ambiguous_kot_multi_source", false)
    } else if let Some(reason) = retry_reason {
        ("weak", reason, true)
    } else if body_coverage == 0.0 {
        ("weak", "no_source_text_support", false)
    } else {
        ("candidates", "matching_fragments_unverified", false)
    };
    Ok(
        json!({"status":status, "detail":detail, "retry_wider":retry_wider,
              "term_coverage":body_coverage}),
    )
}
