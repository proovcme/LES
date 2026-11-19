//! Upgrade OCR and uncertain-layout locators through the original-page reader.
use super::*;
use std::collections::{HashMap, HashSet};
mod units;
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
    let mut pages: HashMap<String, Vec<Value>> = HashMap::new();
    let mut failed = HashSet::new();
    let mut leases: HashMap<String, Option<std::sync::Arc<crate::pdf_text::SourceLease>>> =
        HashMap::new();
    let mut all = points.clone();
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array().into_iter().flatten() {
            all.extend(section["points"].as_array().into_iter().flatten().cloned());
        }
    }
    let mut ranks = HashMap::<String, u64>::new();
    for (i, point) in all
        .iter()
        .enumerate()
        .filter(|(_, p)| p["payload"]["retrieval_expansion"] != true)
    {
        let rank = super::packing::fused_rank(point, i);
        ranks
            .entry(key(point))
            .and_modify(|old| *old = (*old).min(rank))
            .or_insert(rank);
    }
    for point in all {
        let p = &point["payload"];
        let scan = p["ocr"] == true
            || p["pdf_requires_ocr"] == true
            || p["source_layer"] == "pdf_ocr_text";
        let needs_original = scan || p["table_projection_warning"].is_string();
        if !needs_original
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
        let file_key = json!([doc["id"], doc["file_hash"]]).to_string();
        if !leases.contains_key(&file_key) {
            let spec = input.clone();
            // Failure to acquire a read-only lease is never evidence. Use the
            // existing per-page hashes when another program owns a write handle.
            let acquired = tokio::task::spawn_blocking(move || {
                crate::pdf_text::source_lease(&spec).ok().flatten()
            })
            .await
            .map_err(|_| "PDF_READ_FAILED")?;
            leases.insert(file_key.clone(), acquired.map(std::sync::Arc::new));
        }
        let lease = leases[&file_key].clone();
        let full =
            tokio::task::spawn_blocking(move || -> std::result::Result<Vec<Value>, String> {
                let mut input = input;
                let mut full =
                    crate::pdf_text::supervised_source_leased(input.clone(), lease.as_deref())?;
                if full["total_chars"]
                    .as_u64()
                    .is_none_or(|n| n > les_core::context_selection::MAX_SOURCE_TEXT_BYTES as u64)
                {
                    return Err("SOURCE_READ_LIMIT".into());
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
                    full =
                        crate::pdf_text::supervised_source_leased(input.clone(), lease.as_deref())?;
                    if full["offset"] != offset || full["view_sha256"] != view {
                        return Err("SOURCE_VIEW_CHANGED".into());
                    }
                    text.push_str(full["text"].as_str().ok_or("PDF_TEXT_UNAVAILABLE")?);
                    if text.len() > les_core::context_selection::MAX_SOURCE_TEXT_BYTES {
                        return Err("SOURCE_READ_LIMIT".into());
                    }
                }
                if full["total_chars"] != text.chars().count()
                    || view != les_core::sha256_hex(text.as_bytes())
                {
                    return Err("SOURCE_VIEW_CHANGED".into());
                }
                full["text"] = json!(text);
                full["offset"] = json!(0);
                full["complete"] = json!(true);
                units::reading_units(&full).map_err(str::to_owned)
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
        let mut rendered = Vec::with_capacity(full.len());
        for read in full {
            let mut page = point.clone();
            let structured = read["selection"] == "structured_page_unit";
            page["id"] = json!(format!(
                "pdf:{}:{}:{}{}",
                doc["id"].as_str().unwrap_or(""),
                p["page"],
                read["view_sha256"].as_str().unwrap_or(""),
                if structured {
                    format!(
                        ":{}:{}",
                        read["ordinal"],
                        read["reading_unit_sha256"].as_str().unwrap_or("")
                    )
                } else {
                    String::new()
                }
            ));
            page["payload"]["text"] = read["text"].clone();
            page["payload"]["physical_pdf_read"] = read.clone();
            page["payload"]["retrieval_anchor_id"] =
                p.get("retrieval_anchor_id").unwrap_or(&point["id"]).clone();
            page["payload"]["reading_representation"] = json!(if structured {
                "physical_pdf_unit"
            } else {
                "physical_pdf_page"
            });
            page["payload"]["ocr"] = read["ocr"].clone();
            if structured {
                // A page hit is a source prior, not a dense vote for each new
                // paragraph/table unit derived from that page.
                page["payload"]["retrieval_expansion"] = json!(true);
                page["payload"]["retrieval_rank"] = Value::Null;
                page["payload"]["retrieval_source_rank"] = json!(ranks.get(&identity));
                if read["heading"].as_str().is_some_and(|s| !s.is_empty()) {
                    page["payload"]["section_heading"] = read["heading"].clone();
                }
            }
            rendered.push(page);
        }
        pages.insert(identity, rendered);
    }
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array_mut().into_iter().flatten() {
            if let Some(rows) = section["points"].as_array_mut() {
                let mut seen = HashSet::new();
                let mut expanded = HashSet::new();
                *rows = rows
                    .iter()
                    .filter(|p| !failed.contains(&key(p)))
                    .filter(|p| !pages.contains_key(&key(p)) || expanded.insert(key(p)))
                    .flat_map(|p| {
                        pages
                            .get(&key(p))
                            .cloned()
                            .unwrap_or_else(|| vec![p.clone()])
                    })
                    .filter(|p| seen.insert(p["id"].to_string()))
                    .collect();
            }
        }
    }
    let mut seen = HashSet::new();
    let mut expanded = HashSet::new();
    let output = points
        .into_iter()
        .filter(|p| !failed.contains(&key(p)))
        .filter(|p| !pages.contains_key(&key(p)) || expanded.insert(key(p)))
        .flat_map(|p| pages.get(&key(&p)).cloned().unwrap_or_else(|| vec![p]))
        .filter(|p| seen.insert(p["id"].to_string()))
        .collect();
    Ok((output, hydrated))
}
