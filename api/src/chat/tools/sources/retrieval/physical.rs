//! Upgrade indexed OCR locators through the canonical original-page reader.
use super::*;
use std::collections::{HashMap, HashSet};
fn key(point: &Value) -> String {
    let p = &point["payload"];
    json!([
        p["dataset_id"],
        p["file_name"],
        p["source_sha256"],
        p["page"]
    ])
    .to_string()
}
pub(super) async fn hydrate(
    context: &Context,
    turn: &SourceContext,
    points: Vec<Value>,
    mut hydrated: Value,
) -> Result<(Vec<Value>, Value)> {
    let mut pages: HashMap<String, Value> = HashMap::new();
    let mut failed = HashSet::new();
    let mut all = points.clone();
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array().into_iter().flatten() {
            all.extend(section["points"].as_array().into_iter().flatten().cloned());
        }
    }
    for point in all {
        let p = &point["payload"];
        let scan = p["ocr"] == true
            || p["pdf_requires_ocr"] == true
            || p["source_layer"] == "pdf_ocr_text";
        if !scan
            || !p["file_name"]
                .as_str()
                .is_some_and(|s| s.to_ascii_lowercase().ends_with(".pdf"))
            || !p["page"].is_u64()
        {
            continue;
        }
        let identity = key(&point);
        if pages.contains_key(&identity) || failed.contains(&identity) {
            continue;
        }
        let doc = document(
            context,
            &json!({"dataset_id":p["dataset_id"],"file_name":p["file_name"]}),
        )
        .await?;
        scoped_document(turn, &doc)?;
        if doc["file_hash"] != p["source_sha256"] {
            return Err("SOURCE_REVISION_CHANGED".into());
        }
        let input = json!({"kind":"source_page","database":context.database,"content_root":context.state_root.join("storage/datasets"),
            "source":{"doc_id":doc["id"],"dataset_id":doc["dataset_id"],"source_sha256":doc["file_hash"],"page":p["page"],"max_chars":12000}});
        let full = tokio::task::spawn_blocking(move || -> std::result::Result<Value, String> {
            let mut input = input;
            let mut full = crate::pdf_text::supervised_source(input.clone())?;
            if full["total_chars"].as_u64().is_none_or(|n| n > 16000) {
                return Err("SOURCE_ATOMIC_UNIT_OVERFLOW".into());
            }
            let mut text = full["text"]
                .as_str()
                .ok_or("PDF_TEXT_UNAVAILABLE")?
                .to_owned();
            let view = full["view_sha256"].clone();
            while let Some(offset) = full["next_offset"].as_u64() {
                if offset != text.chars().count() as u64 {
                    return Err("PDF_OFFSET_INVALID".into());
                }
                input["source"]["offset"] = json!(offset);
                input["source"]["view_sha256"] = view.clone();
                full = crate::pdf_text::supervised_source(input.clone())?;
                if full["offset"] != offset || full["view_sha256"] != view {
                    return Err("SOURCE_VIEW_CHANGED".into());
                }
                text.push_str(full["text"].as_str().ok_or("PDF_TEXT_UNAVAILABLE")?);
            }
            if full["total_chars"] != text.chars().count()
                || view != les_core::sha256_hex(text.as_bytes())
            {
                return Err("SOURCE_VIEW_CHANGED".into());
            }
            full["text"] = json!(text);
            full["offset"] = json!(0);
            full["complete"] = json!(true);
            Ok(full)
        })
        .await
        .map_err(|_| "PDF_READ_FAILED")?;
        let full = match full {
            Ok(value) => value,
            Err(code)
                if super::failures::record(
                    &mut hydrated,
                    reference(&doc, p["page"].clone()),
                    &code,
                ) =>
            {
                failed.insert(identity);
                continue;
            }
            Err(code) => return Err(code.into()),
        };
        let mut page = point.clone();
        page["id"] = json!(format!(
            "pdf:{}:{}:{}",
            doc["id"].as_str().unwrap_or(""),
            p["page"],
            full["view_sha256"].as_str().unwrap_or("")
        ));
        page["payload"]["text"] = full["text"].clone();
        page["payload"]["physical_pdf_read"] = full.clone();
        page["payload"]["retrieval_anchor_id"] = point["id"].clone();
        page["payload"]["reading_representation"] = json!("physical_pdf_page");
        page["payload"]["ocr"] = full["ocr"].clone();
        pages.insert(identity, page);
    }
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array_mut().into_iter().flatten() {
            if let Some(rows) = section["points"].as_array_mut() {
                let mut seen = HashSet::new();
                *rows = rows
                    .iter()
                    .filter(|p| !failed.contains(&key(p)))
                    .map(|p| pages.get(&key(p)).unwrap_or(p).clone())
                    .filter(|p| seen.insert(p["id"].to_string()))
                    .collect();
            }
        }
    }
    let mut seen = HashSet::new();
    let output = points
        .into_iter()
        .filter(|p| !failed.contains(&key(p)))
        .map(|p| pages.get(&key(&p)).unwrap_or(&p).clone())
        .filter(|p| seen.insert(p["id"].to_string()))
        .collect();
    Ok((output, hydrated))
}
