//! A failed conversion is an unread source, never proof or a global empty search.
use serde_json::{json, Value};

pub(super) fn recoverable(code: &str) -> bool {
    matches!(
        code,
        "WORKER_TIMEOUT"
            | "WORKER_QUEUE_TIMEOUT"
            | "NO_SEARCHABLE_TEXT"
            | "SOURCE_ATOMIC_UNIT_OVERFLOW"
            | "PDF_TEXT_UNAVAILABLE"
            | "DOCUMENT_ARCHIVE_INVALID"
            | "DOCUMENT_ARCHIVE_LIMIT"
            | "DOCUMENT_FILE_LIMIT"
            | "DOCUMENT_XML_INVALID"
            | "DOCUMENT_XML_LIMIT"
            | "DOCUMENT_TEXT_LIMIT"
    ) || code.starts_with("OCR_")
        || code.starts_with("DOCX_")
        || code.starts_with("DOCLING_WORD_")
}
pub(in crate::chat::tools::sources) fn record(
    hydrated: &mut Value,
    source: Value,
    code: &str,
) -> bool {
    if !recoverable(code) {
        return false;
    }
    let reader = if source["page"].is_u64() {
        "read_pdf_source"
    } else {
        "read_source"
    };
    if !hydrated["read_failures"].is_array() {
        hydrated["read_failures"] = json!([]);
    }
    hydrated["read_failures"].as_array_mut().unwrap().push(json!({"source":source,"code":code,
        "message":"Оригинал не дочитан автоматически. Его нужно прочитать отдельно; отказ не означает, что в нём нет ответа.",
        "evidence_admissible":false,"read_required":reader}));
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_timeout_is_local_but_scope_and_revision_failures_abort_the_query() {
        let mut result = json!({});
        assert!(record(
            &mut result,
            json!({"doc_id":"failed"}),
            "WORKER_TIMEOUT"
        ));
        assert!(record(
            &mut result,
            json!({"doc_id":"bad-archive"}),
            "DOCUMENT_ARCHIVE_INVALID"
        ));
        for code in [
            "SOURCE_REVISION_CHANGED",
            "TOOL_SCOPE_VIOLATION",
            "INDEX_CHANGED_DURING_SEARCH",
        ] {
            assert!(!record(&mut result, Value::Null, code));
        }
        assert_eq!(result["read_failures"].as_array().unwrap().len(), 2);
        assert_eq!(result["read_failures"][0]["evidence_admissible"], false);
    }
}
