//! Search metadata is not quoted document content. Dense and BM25 use one input.
use serde_json::Value;
pub fn input(payload: &Value, body: &str) -> String {
    if payload["search_context_schema"] == "les.search-context.v1" {
        format!(
            "{}{body}",
            payload["search_context_prefix"].as_str().unwrap_or("")
        )
    } else {
        body.to_owned()
    }
}
pub fn cache_key(payload: &Value) -> Option<&str> {
    payload
        .get("embedding_content_hash")
        .unwrap_or(&payload["content_hash"])
        .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn search_metadata_never_replaces_a_quote_and_has_a_separate_cache_identity() {
        let a = json!({"search_context_schema":"les.search-context.v1","search_context_prefix":"Document: A\n","content_hash":"body","embedding_content_hash":"input-a"});
        let b = json!({"search_context_schema":"les.search-context.v1","search_context_prefix":"Document: B\n","content_hash":"body","embedding_content_hash":"input-b"});
        assert_ne!(
            input(&a, "|Quantity|1|Mass|69|"),
            input(&b, "|Quantity|1|Mass|69|")
        );
        assert_ne!(cache_key(&a), cache_key(&b));
        assert_eq!(a["content_hash"], b["content_hash"]);
        assert_eq!(
            input(&json!({}), "Original paragraph"),
            "Original paragraph"
        );
        assert_eq!(cache_key(&json!({"content_hash":"legacy"})), Some("legacy"));
    }
}
