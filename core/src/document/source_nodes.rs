//! Source-local nodes before final budgets/publication. No filesystem or models.
use super::{
    chunks,
    nodes::{present, MAX_NODES},
    tokenizer,
};
use regex::Regex;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::{collections::HashSet, sync::LazyLock};

type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "DOCUMENT_SOURCE_NODES_INVALID";
static PAGES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^## (?:Page|Стр\.)[\s\x1c-\x1f]+([0-9]+)[\s\x1c-\x1f]*$").unwrap()
});

pub(super) fn uuid5(value: &str) -> String {
    let mut digest = Sha1::new();
    // RFC 9562 URL namespace, retained from Python uuid.NAMESPACE_URL.
    digest.update([
        0x6b, 0xa7, 0xb8, 0x11, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
    ]);
    digest.update(value.as_bytes());
    let mut bytes: [u8; 16] = digest.finalize()[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn pages(text: &str) -> Result<Vec<(u64, &str)>> {
    let markers = PAGES.captures_iter(text).take(10_001).collect::<Vec<_>>();
    if markers.len() > 10_000 {
        return Err("PDF_PAGE_LIMIT");
    }
    let mut result = Vec::new();
    for (index, marker) in markers.iter().enumerate() {
        let page = marker[1].parse::<u64>().map_err(|_| INVALID)?;
        if !(1..=10_000).contains(&page) {
            return Err("PDF_PAGE_LIMIT");
        }
        let end = markers
            .get(index + 1)
            .map_or(text.len(), |v| v.get(0).unwrap().start());
        result.push((page, chunks::trim(&text[marker.get(0).unwrap().end()..end])));
    }
    Ok(result)
}

pub(super) fn page_blocks(request: &Value) -> Result<Value> {
    Ok(json!({"pages":pages(request["text"].as_str().ok_or(INVALID)?)?}))
}

fn integer(config: &Value, key: &str, minimum: u64, maximum: u64) -> Result<usize> {
    config[key]
        .as_u64()
        .filter(|v| (minimum..=maximum).contains(v))
        .map(|v| v as usize)
        .ok_or(INVALID)
}
fn payload(request: &Value, value: Map<String, Value>) -> Result<Value> {
    let mut merged = Map::new();
    if let Some(route) = request.get("route_metadata") {
        merged.extend(route.as_object().ok_or(INVALID)?.clone());
        merged.remove("pdf_page_views");
    }
    // Source identity wins over classification annotations. A route cannot
    // silently change the physical page or turn a projection into another type.
    merged.extend(value);
    merged.insert(
        "source_node_builder".into(),
        json!("rust.document.source-nodes.v1"),
    );
    Ok(Value::Object(merged))
}
fn push(out: &mut Vec<Value>, node: Value) -> Result<()> {
    if out.len() >= MAX_NODES {
        return Err("INDEX_FILE_CHUNK_LIMIT");
    }
    out.push(node);
    Ok(())
}
fn from_pdf(
    request: &Value,
    text: &str,
    bridge: &impl Fn(u8, &Value) -> Result<Value>,
) -> Result<Vec<Value>> {
    let file = request["file_key"].as_str().ok_or(INVALID)?;
    let dataset = request["dataset_id"].as_str().ok_or(INVALID)?;
    let size = integer(request, "size", 1, 1_000_000)?;
    let overlap = integer(request, "overlap", 0, size as u64 - 1)?.min(size / 3);
    let blocks = pages(text)?;
    let mut seen = HashSet::new();
    for (page, _) in &blocks {
        if !seen.insert(*page) {
            return Err("PDF_PAGE_DUPLICATE");
        }
    }
    let mut passports = Map::new();
    if request["passports"] == true {
        for batch in blocks.chunks(64) {
            let reply = bridge(
                2,
                &json!({"pages":batch.iter().map(|v|v.0).collect::<Vec<_>>()}),
            )?;
            if reply.get("error").is_some() {
                return Err("DOCUMENT_SOURCE_PASSPORT_FAILED");
            }
            passports.extend(reply["passports"].as_object().ok_or(INVALID)?.clone());
        }
    }
    let mut out = Vec::new();
    for (page, text) in blocks {
        if text.is_empty() || text == "[no text extracted]" {
            continue;
        }
        let parts = chunks::execute(
            &json!({"text":text,"kind":"page","size":size,"overlap":overlap}),
            None,
        )?;
        let parts = parts["chunks"].as_array().ok_or(INVALID)?;
        let passport = passports
            .get(&page.to_string())
            .filter(|v| v.is_object() && present(v));
        for (index, part) in parts.iter().enumerate() {
            let part = part.as_str().ok_or(INVALID)?;
            let number = index + 1;
            let title = if parts.len() > 1 {
                format!("## Page {page} part {number}/{}", parts.len())
            } else {
                format!("## Page {page}")
            };
            let reference = passport
                .and_then(|p| p.get("source_ref"))
                .filter(|v| present(v))
                .cloned()
                .unwrap_or_else(|| json!(format!("{file}#page={page}")));
            let ocr =
                request["ocr"] == true || passport.is_some_and(|p| present(&p["requires_ocr"]));
            let mut meta = json!({"type":"pdf_page_text","source_layer":if ocr {"pdf_ocr_text"} else {"pdf_text_layer"},
                "page":page,"page_part":number,"page_parts":parts.len(),"source_ref":reference}).as_object().unwrap().clone();
            if index == 0 {
                if let Some(view) = request["route_metadata"]["pdf_page_views"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|v| v["page"] == page)
                    .and_then(|v| v["text"].as_str())
                    .filter(|v| !v.is_empty() && v.chars().count() <= 16000)
                {
                    meta.insert("pdf_page_view".into(), json!(view));
                    meta.insert(
                        "pdf_page_view_sha256".into(),
                        json!(crate::sha256_hex(view.as_bytes())),
                    );
                    meta.insert("pdf_page_view_schema".into(), json!("les.pdf-page-view.v1"));
                }
            }
            if let Some(p) = passport {
                for (key, value) in [
                    ("pdf_page_passport_schema", &p["schema"]),
                    ("pdf_page_type", &p["page_type"]),
                    ("pdf_page_type_label", &p["page_type_label"]),
                    ("pdf_routing_confidence", &p["routing_confidence"]),
                    ("pdf_stamp_status", &p["stamp"]["status"]),
                ] {
                    meta.insert(key.into(), value.clone());
                }
                meta.insert(
                    "pdf_requires_ocr".into(),
                    json!(present(&p["requires_ocr"])),
                );
                for (target, source) in [
                    ("pdf_recognition_quality", "recognition_quality"),
                    ("pdf_page_signals", "signals"),
                ] {
                    meta.insert(
                        target.into(),
                        p[source]
                            .as_object()
                            .cloned()
                            .map(Value::Object)
                            .unwrap_or_else(|| json!({})),
                    );
                }
                meta.insert(
                    "pdf_sheet_number".into(),
                    if present(&p["stamp"]["sheet_number"]) {
                        p["stamp"]["sheet_number"].clone()
                    } else {
                        json!("")
                    },
                );
                let fragments = p["evidence_fragments"].as_array().into_iter().flatten().filter(|v|v.is_object()).take(5)
                    .map(|v|json!({"fragment_id":v["fragment_id"],"bbox":v["bbox"],"source_ref":v["source_ref"]})).collect::<Vec<_>>();
                meta.insert("pdf_fragment_bboxes".into(), json!(fragments));
            }
            push(
                &mut out,
                json!({"text":format!("{title}\n\n{}",chunks::trim(part)),
                "doc_id":uuid5(&format!("{dataset}:{file}:pdf-page:{page}:{number}")),"payload":payload(request,meta)?}),
            )?;
        }
    }
    Ok(out)
}

fn from_markdown(request: &Value, text: &str) -> Result<Vec<Value>> {
    let file = request["file_key"].as_str().ok_or(INVALID)?;
    let dataset = request["dataset_id"].as_str().ok_or(INVALID)?;
    let size = integer(request, "size", 1, 1_000_000)?;
    let overlap = integer(request, "overlap", 0, size as u64 - 1)?;
    if file.to_ascii_lowercase().ends_with(".docx") || file.to_ascii_lowercase().ends_with(".doc") {
        // A readable source unit is independent of embedding/token budgets.
        // The final builder may split its search representation, retaining one
        // complete, hashed reading anchor for all children.
        return super::structure::units(text, 4000)?.into_iter().map(|unit| {
            let ordinal=unit["ordinal"].as_u64().ok_or(INVALID)?;
            let body=unit["text"].as_str().ok_or(INVALID)?;
            let mut meta=json!({"type":"word_structure","word_reading_view":unit,
                "word_reading_schema":"les.document-reading.v1",
                "word_reading_sha256":crate::sha256_hex(body.as_bytes()),
                "section_heading":unit["heading"],"source_unit_id":ordinal})
                .as_object().unwrap().clone();
            meta.insert("source_ref".into(),json!(format!("{file}#unit={ordinal}")));
            Ok(json!({"text":body,"doc_id":uuid5(&format!("{dataset}:{file}:word-v1:{ordinal}:{body}")),
                "payload":payload(request,meta)?}))
        }).collect();
    }
    let _minimum = integer(request, "minimum", 0, 1_000_000)?;
    let _final_minimum = integer(request, "final_minimum", 0, 1_000_000)?;
    let local = match request["unit"].as_str() {
        Some("tokens") => Some(tokenizer::load(&request["tokenizer"])?),
        Some("chars") if !request["tokenizer"].is_object() => None,
        _ => return Err(INVALID),
    };
    let sections = chunks::markdown_sections(&json!({"text":text}))?;
    // A verified source clause/title is evidence even below a noise floor.
    // Keep section boundaries and identities; the final builder owns budgets.
    let mut out = Vec::new();
    for (section_index, section) in sections["chunks"]
        .as_array()
        .ok_or(INVALID)?
        .iter()
        .enumerate()
    {
        let section = section.as_str().ok_or(INVALID)?;
        if !section.chars().any(char::is_alphanumeric) {
            continue;
        }
        let parts = if section.chars().count() > 2000 {
            let config = json!({"text":section,"size":size,"overlap":overlap,"kind":"structured"});
            if let Some(local) = local.as_ref() {
                chunks::execute(&config, Some(&|v| local.count(v)))?
            } else {
                chunks::execute(&config, None)?
            }
        } else {
            json!({"chunks":[section]})
        };
        for (part_index, part) in parts["chunks"]
            .as_array()
            .ok_or(INVALID)?
            .iter()
            .enumerate()
        {
            let part = part.as_str().ok_or(INVALID)?;
            if chunks::trim(part).is_empty() {
                continue;
            }
            let id = uuid5(&format!(
                "{dataset}:{file}:markdown:{section_index}:{part_index}:{part}"
            ));
            let meta = json!({"type":request["payload_type"].as_str().unwrap_or("markdown")})
                .as_object()
                .unwrap()
                .clone();
            push(
                &mut out,
                json!({"text":part,"doc_id":id,"payload":payload(request,meta)?}),
            )?;
        }
    }
    Ok(out)
}

pub(crate) fn run(request: &Value, bridge: impl Fn(u8, &Value) -> Result<Value>) -> Result<Value> {
    if request["schema"] != "les.document-source-nodes.v1" {
        return Err(INVALID);
    }
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let reply = bridge(0, &json!({"offset":offset,"limit":262144}))?;
        let part = reply["text"].as_str().ok_or(INVALID)?;
        let count = part.chars().count();
        if count > 262144 || (reply["complete"] != true && count != 262144) {
            return Err(INVALID);
        }
        if text.len() + part.len() > 200 * 1024 * 1024 {
            return Err("DOCUMENT_TEXT_LIMIT");
        }
        text.push_str(part);
        offset += count;
        if reply["complete"] == true {
            break;
        }
    }
    let nodes = match request["kind"].as_str() {
        Some("pdf") => from_pdf(request, &text, &bridge)?,
        Some("markdown") => from_markdown(request, &text)?,
        _ => return Err(INVALID),
    };
    // Complete preparation before exposing a partial result to a caller.
    let mut batch = Vec::new();
    let mut size = 0;
    let mut total = 0;
    for node in &nodes {
        let bytes = serde_json::to_vec(node).map_err(|_| INVALID)?.len();
        if bytes > 8 * 1024 * 1024 {
            return Err("INDEX_POINT_SIZE_LIMIT");
        }
        total += bytes;
        if total > 512 * 1024 * 1024 {
            return Err("INDEX_FILE_CHUNK_LIMIT");
        }
        if !batch.is_empty() && (size + bytes > 4 * 1024 * 1024 || batch.len() >= 64) {
            if bridge(1, &json!({"nodes":batch}))?["ok"] != true {
                return Err(INVALID);
            }
            batch.clear();
            size = 0;
        }
        batch.push(node);
        size += bytes;
    }
    if !batch.is_empty() && bridge(1, &json!({"nodes":batch}))?["ok"] != true {
        return Err(INVALID);
    }
    Ok(json!({"count":nodes.len(),"owner":"rust.document.source-nodes","status":"completed"}))
}
