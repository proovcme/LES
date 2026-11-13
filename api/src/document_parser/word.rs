//! Snapshot-backed Word parser requests.
use super::*;
pub(crate) fn word(input: &Value, bytes: &[u8]) -> Result<Value> {
    let hash = les_core::sha256_hex(bytes);
    if input["prepared_sha256"].as_str() != Some(hash.as_str()) {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    let root = state(Some(input))?;
    let root = folder(&root, "storage/parser-tmp")?;
    let snapshot = temporary(&root, bytes)?;
    let value = invoke(
        json!({"schema":"les.document-parsers.v1","action":"word","format":"docx","path":snapshot.0,"sha256":hash,"slot_prefix":input["slot_prefix"]}),
    )?;
    if value["source_sha256"] != hash {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    Ok(value)
}
pub(crate) fn legacy_word(input: &Value) -> Result<Value> {
    let path = Path::new(input["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?);
    let bytes = bounded(path, 64 * 1024 * 1024)?;
    let hash = les_core::sha256_hex(&bytes);
    if input["source_sha256"].as_str() != Some(hash.as_str()) {
        return Err("SOURCE_CHANGED_DURING_INDEXING");
    }
    let root = folder(&state(Some(input))?, "storage/parser-tmp")?;
    let snapshot = temporary(&root, &bytes)?;
    let value = invoke(
        json!({"schema":"les.document-parsers.v1","action":"word","format":"doc","path":snapshot.0,"sha256":hash}),
    )?;
    Ok(json!({"text":value["text"],"metadata":{"extractor":value["extractor"]}}))
}
