//! Read-only format adapters. No model, indexing or source writes.
mod legacy_doc;
mod plain_text;
pub(crate) mod workbook;
pub(crate) mod workbook_index;
use serde_json::{json, Value};

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    match request["action"].as_str() {
        Some("plain_text") => {
            let path = request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?;
            plain_text::read(std::path::Path::new(path))
        }
        Some("legacy_doc") => {
            let path = request["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?;
            let text = legacy_doc::read(std::path::Path::new(path))?;
            Ok(json!({"text":text,"extractor":"les.ms-doc.v1"}))
        }
        _ => Err("DOCUMENT_ACTION_INVALID"),
    }
}
