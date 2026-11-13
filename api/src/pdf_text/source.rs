//! Original-backed snapshots: one OCR run per page, stable Unicode cursors.
use super::*;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};
static CACHE: OnceLock<Mutex<VecDeque<(String, Value)>>> = OnceLock::new();
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

pub(super) fn read(mut input: Value) -> Result<Value, String> {
    let original = les_core::pdf_source::resolve(&input)?;
    // A warm cache never conceals a missing/replaced original.
    les_core::pdf_source::verify(
        &mut File::open(&original.path).map_err(|_| "PDF_SOURCE_UNAVAILABLE")?,
        &original.sha256,
    )?;
    let key = json!([
        input["database"],
        original.sha256,
        input["source"]["page"],
        std::env::var_os("LES_NATIVE_DOCUMENT_PARSERS"),
        "les.pdf-read.v3"
    ])
    .to_string();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "PDF_READ_FAILED")?;
    let mut value = if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
        let entry = cache.remove(i).unwrap();
        let value = entry.1.clone();
        cache.push_back(entry);
        value
    } else {
        let args = input["source"].clone();
        input["source"]
            .as_object_mut()
            .ok_or("PDF_SOURCE_INVALID")?
            .remove("q");
        input["source"]["offset"] = json!(0);
        input["snapshot"] = json!(true);
        let full = match worker_source(input.clone()) {
            Err(code) if code == "PDF_OCR_REQUIRED" => {
                let state = input["content_root"]
                    .as_str()
                    .map(Path::new)
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .map(Path::to_path_buf);
                let mut ocr = crate::document_dispatch::configured_ocr(state.as_deref())?;
                ocr["timeout_ms"] = json!(60000);
                input["ocr"] = ocr;
                let result = worker_source(input.clone());
                if let Some(state) = state.as_deref() {
                    crate::pdf_ocr::cleanup(state);
                }
                result?
            }
            result => result?,
        };
        input["source"] = args;
        let size = full.to_string().len();
        while cache.len() >= 32
            || cache
                .iter()
                .map(|(_, v)| v.to_string().len())
                .sum::<usize>()
                + size
                > MAX_CACHE_BYTES
        {
            if cache.pop_front().is_none() {
                break;
            }
        }
        if size <= MAX_CACHE_BYTES {
            cache.push_back((key, full.clone()));
        }
        full
    };
    let text = value["text"]
        .as_str()
        .ok_or("PDF_TEXT_UNAVAILABLE")?
        .to_owned();
    let hash = les_core::sha256_hex(text.as_bytes());
    if input["source"]["view_sha256"]
        .as_str()
        .is_some_and(|s| s != hash)
    {
        return Err("SOURCE_VIEW_CHANGED".into());
    }
    let query = input["source"]["q"].as_str().unwrap_or("").trim();
    let offset = input["source"]
        .get("offset")
        .map_or(Some(0), Value::as_u64)
        .ok_or("PDF_OFFSET_INVALID")?;
    if offset > 0 && value["ocr"] == true && input["source"]["view_sha256"].as_str().is_none() {
        return Err("SOURCE_CONTINUATION_VIEW_REQUIRED".into());
    }
    let limit = input["source"]
        .get("max_chars")
        .map_or(Some(6000), Value::as_u64)
        .filter(|n| (1..=12000).contains(n))
        .ok_or("PDF_TEXT_LIMIT")?;
    let (start, end, selection) = source_window(&text, query, offset, limit)?;
    value["text"] = json!(text
        .chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect::<String>());
    value["offset"] = json!(start);
    value["next_offset"] = if end < text.chars().count() as u64 {
        json!(end)
    } else {
        Value::Null
    };
    value["total_chars"] = json!(text.chars().count());
    value["view_sha256"] = json!(hash);
    value["selection"] = json!(selection);
    value["matched_query"] = json!(query);
    value["complete"] = json!(start == 0 && end == text.chars().count() as u64);
    for (k, v) in original
        .identity
        .as_object()
        .ok_or("PDF_IDENTITY_REQUIRED")?
    {
        value[k] = v.clone();
    }
    les_core::pdf_source::verify(
        &mut File::open(&original.path).map_err(|_| "PDF_SOURCE_UNAVAILABLE")?,
        &original.sha256,
    )?;
    Ok(value)
}
