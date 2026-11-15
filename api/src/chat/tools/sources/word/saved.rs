//! Published canonical units are immutable within a checked source/index revision.
use super::*;
use std::collections::{BTreeMap, HashSet};

pub(super) fn enabled() -> Result<bool> {
    match std::env::var("LES_EXPERIMENT_SAVED_READING").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("") | Ok("original-v1") => Ok(false),
        Ok("canonical-v1") => Ok(true),
        _ => Err("SAVED_READING_EXPERIMENT_INVALID".into()),
    }
}

pub(super) async fn load(
    context: &Context,
    doc: &Value,
    collection: &str,
) -> Result<Option<Value>> {
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let revision = store.revision(context, collection).await?;
    let original = doc.clone();
    let root = context.state_root.clone();
    tokio::task::spawn_blocking(move || verify_at(&root, &original))
        .await
        .map_err(|_| "SOURCE_READ_FAILED")??;
    let filter = json!({"must":[{"key":"dataset_id","match":{"value":doc["dataset_id"]}},
        {"key":"file_name","match":{"value":doc["file_name"]}}],
        "must_not":[{"key":"node_role","match":{"value":"navigation"}},{"key":"evidence_admissible","match":{"value":false}}]});
    let mut units = BTreeMap::<u64, Value>::new();
    let mut expected = HashSet::new();
    let mut offsets = HashSet::new();
    let mut offset = Value::Null;
    let mut bytes = 0usize;
    let mut points = 0usize;
    let mut metadata = json!({"extractor":"published-canonical-word","ocr":false,"visual_content":false,"unread_visual_count":0,"visual_coverage":"complete"});
    loop {
        let mut request = json!({"filter":filter,"limit":128,"with_vector":false,
            "with_payload":["dataset_id","file_name","source_sha256","source_unit_id","word_reading_schema","word_reading_view","word_reading_sha256","word_structure_sha256","ocr","visual_content","unread_visual_count","visual_coverage"]});
        if !offset.is_null() {
            request["offset"] = offset.clone();
        }
        let page = store
            .read_owned(context, collection, "scroll", request)
            .await?;
        for row in page["points"].as_array().ok_or("INDEX_READ_INVALID")? {
            points += 1;
            if points > 100_000 {
                return Err("SOURCE_READ_LIMIT".into());
            }
            let p = &row["payload"];
            if p["dataset_id"] != doc["dataset_id"] || p["file_name"] != doc["file_name"] {
                return Err("TOOL_SCOPE_VIOLATION".into());
            }
            if p["source_sha256"] != doc["file_hash"] {
                return Err("INDEX_CHANGED_DURING_SEARCH".into());
            }
            if p["word_reading_schema"] != "les.document-reading.v1" {
                return Ok(None);
            }
            let ordinal = p["source_unit_id"]
                .as_u64()
                .ok_or("WORD_READING_VIEW_INVALID")?;
            expected.insert(ordinal);
            for key in ["ocr", "visual_content"] {
                if p[key] == true {
                    metadata[key] = json!(true);
                }
            }
            metadata["unread_visual_count"] = json!(
                metadata["unread_visual_count"]
                    .as_u64()
                    .unwrap_or(0)
                    .max(p["unread_visual_count"].as_u64().unwrap_or(0))
            );
            if p["visual_coverage"] == "partial" {
                metadata["visual_coverage"] = json!("partial");
            }
            if let Some(unit) = p.get("word_reading_view") {
                if !unit["canonical"].is_object() && p["word_structure_sha256"].is_null() {
                    return Ok(None);
                }
                validate(p, unit)?;
                if let Some(previous) = units.get(&ordinal) {
                    if previous != unit {
                        return Err("WORD_READING_VIEW_INVALID".into());
                    }
                } else {
                    bytes += unit.to_string().len();
                    if bytes > les_core::context_selection::MAX_SOURCE_TEXT_BYTES
                        || units.len() >= les_core::context_selection::MAX_SOURCE_UNITS
                    {
                        return Err("SOURCE_READ_LIMIT".into());
                    }
                    units.insert(ordinal, unit.clone());
                }
            }
        }
        offset = page["next_page_offset"].clone();
        if offset.is_null() {
            break;
        }
        if !offsets.insert(offset.to_string()) {
            return Err("INDEX_CHANGED_DURING_SEARCH".into());
        }
    }
    if units.is_empty() || expected.iter().any(|i| !units.contains_key(i)) {
        return Ok(None);
    }
    if store.revision(context, collection).await? != revision {
        return Err("INDEX_CHANGED_DURING_SEARCH".into());
    }
    let original = doc.clone();
    let root = context.state_root.clone();
    tokio::task::spawn_blocking(move || verify_at(&root, &original))
        .await
        .map_err(|_| "SOURCE_READ_FAILED")??;
    let units: Vec<_> = units.into_values().collect();
    if metadata["unread_visual_count"].as_u64().unwrap_or(0) > 0 {
        metadata["visual_coverage"] = json!("partial");
    }
    let identity = json!([
        "les.saved-word-view.v1",
        doc["dataset_id"],
        doc["file_name"],
        doc["file_hash"],
        units
    ]);
    let hash =
        les_core::sha256_hex(les_core::tool_execution::canonical_json(&identity)?.as_bytes());
    eprintln!(
        "{}",
        json!({"event":"saved_word_read_completed","doc_id":doc["id"],"units":units.len(),"source_sha256":doc["file_hash"]})
    );
    Ok(Some(
        json!({"schema":"les.document-reading.v1","view_sha256":hash,"units":units,"metadata":metadata,"saved_units":true,"index_revision":revision}),
    ))
}

fn validate(p: &Value, unit: &Value) -> Result<()> {
    if !unit["canonical"].is_object()
        || unit["ordinal"] != p["source_unit_id"]
        || p["word_reading_sha256"]
            != les_core::sha256_hex(
                unit["text"]
                    .as_str()
                    .ok_or("WORD_READING_VIEW_INVALID")?
                    .as_bytes(),
            )
        || p["word_structure_sha256"]
            != les_core::sha256_hex(les_core::tool_execution::canonical_json(unit)?.as_bytes())
    {
        return Err("WORD_READING_VIEW_INVALID".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_units_bind_literal_text_cells_and_original_ordinal() {
        let unit = json!({"ordinal":7,"text":"Pump A 17.25 kW","canonical":{"cells":["Pump A","17.25","kW"]}});
        let p = json!({"source_unit_id":7,"word_reading_sha256":les_core::sha256_hex(unit["text"].as_str().unwrap().as_bytes()),"word_structure_sha256":les_core::sha256_hex(les_core::tool_execution::canonical_json(&unit).unwrap().as_bytes())});
        validate(&p, &unit).unwrap();
        let mut altered = unit.clone();
        altered["canonical"]["cells"][1] = json!("117.25");
        assert!(validate(&p, &altered).is_err());
        altered = unit.clone();
        altered["ordinal"] = json!(8);
        assert!(validate(&p, &altered).is_err());
    }
}
