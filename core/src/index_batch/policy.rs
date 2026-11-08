use serde_json::Value;
use std::{collections::HashMap, sync::LazyLock};

static MESSAGES: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../config/extraction-errors.json"))
        .expect("checked extraction error messages")
});

pub(super) fn message(code: &str) -> &str {
    MESSAGES.get(code).map(String::as_str).unwrap_or(code)
}
pub(super) fn classify(error: &Value, attempts: u64, max_attempts: u64) -> (String, f64) {
    let code = error["code"].as_str().unwrap_or("");
    let kind = error["error_type"].as_str().unwrap_or("");
    let message = error["message"].as_str().unwrap_or(code).to_lowercase();
    if kind == "UnsupportedIndexingSourceError" || code == "UNSUPPORTED_INDEXING_SOURCE" {
        return ("UNSUPPORTED_INDEXING_SOURCE".into(), 0.);
    }
    let transient_code = matches!(
        code,
        "WORKER_TIMEOUT"
            | "WORKER_QUEUE_TIMEOUT"
            | "WORKER_EXITED"
            | "WORKER_CANCELLED"
            | "WORKER_START_FAILED"
            | "INDEX_STORAGE_WRITE_FAILED"
            | "INDEX_CATALOG_WRITE_FAILED"
            | "INDEX_UPDATE_BUSY"
            | "SOURCE_CHANGED_DURING_INDEXING"
            | "EMBEDDING_COUNT_MISMATCH"
            | "INDEX_SPOOL_WRITE_FAILED"
    );
    let machine_code = !code.is_empty()
        && code.len() <= 120
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    let known = machine_code
        || transient_code
        || code.starts_with("DOC_")
        || code.starts_with("OCR_")
        || code.starts_with("INDEX_SOURCE_")
        || matches!(
            code,
            "WORKER_OUTPUT_LIMIT"
                | "WORKER_RESPONSE_INVALID"
                | "WORKER_RESPONSE_INCOMPLETE"
                | "FORMAT_READ_FAILED"
                | "NO_SEARCHABLE_TEXT"
                | "EMBEDDING_VECTOR_INVALID"
                | "INDEX_FILE_CHUNK_LIMIT"
                | "INDEX_POINT_SIZE_LIMIT"
                | "INDEX_POINT_BATCH_LIMIT"
                | "INDEX_POINTS_INVALID"
                | "INDEX_POINT_ID_UNAVAILABLE"
                | "INDEX_POINTS_ADAPTER_FAILED"
        );
    let markers = [
        (
            "missing prevalidated sparse vector",
            "SPARSE_VECTOR_PREVALIDATION_MISSING",
        ),
        ("qdrant point count mismatch", "QDRANT_POINT_COUNT_MISMATCH"),
        ("qdrant_point_count_mismatch", "QDRANT_POINT_COUNT_MISMATCH"),
        ("index_storage_write_failed", "INDEX_STORAGE_WRITE_FAILED"),
        ("index_catalog_write_failed", "INDEX_CATALOG_WRITE_FAILED"),
        ("index_update_busy", "INDEX_UPDATE_BUSY"),
        (
            "source_changed_during_indexing",
            "SOURCE_CHANGED_DURING_INDEXING",
        ),
        ("embedding count mismatch", "EMBEDDING_COUNT_MISMATCH"),
        ("embedding_count_mismatch", "EMBEDDING_COUNT_MISMATCH"),
        ("index_spool_write_failed", "INDEX_SPOOL_WRITE_FAILED"),
        ("timed out", "PARSE_TIMEOUT"),
        ("timeout", "PARSE_TIMEOUT"),
        ("temporarily unavailable", "DEPENDENCY_UNAVAILABLE"),
        ("all connection attempts failed", "DEPENDENCY_UNAVAILABLE"),
        ("connection", "DEPENDENCY_UNAVAILABLE"),
    ];
    let marker = markers.iter().find(|(marker, _)| message.contains(marker));
    let selected = if known {
        code
    } else {
        marker.map_or("PARSE_FAILED", |(_, code)| *code)
    };
    let transient = if known {
        transient_code
    } else {
        marker.is_some()
    };
    let retry = if transient && attempts < max_attempts {
        super::store::now() + (5. * 2_f64.powi(attempts.saturating_sub(1).min(8) as i32)).min(300.)
    } else {
        0.
    };
    (selected.into(), retry)
}
