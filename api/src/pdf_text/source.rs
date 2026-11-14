//! Original-backed snapshots: one OCR run per page, stable Unicode cursors.
use super::*;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};
static CACHE: OnceLock<Mutex<VecDeque<(String, Value)>>> = OnceLock::new();
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

/// A request-scoped Windows handle that disallows writes and replacement.
/// Only this typed guard may substitute repeated hashes of the same original.
pub(crate) struct ReadLease {
    path: std::path::PathBuf,
    sha256: String,
    _file: File,
}
impl ReadLease {
    fn open(original: les_core::pdf_source::Source) -> Result<Option<Self>, String> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let Ok(mut file) = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(&original.path)
            else {
                return Ok(None);
            };
            les_core::pdf_source::verify(&mut file, &original.sha256)?;
            Ok(Some(Self {
                path: original.path,
                sha256: original.sha256,
                _file: file,
            }))
        }
        #[cfg(not(windows))]
        {
            let _ = original;
            Ok(None)
        }
    }
    fn matches(&self, original: &les_core::pdf_source::Source) -> bool {
        self.path == original.path && self.sha256 == original.sha256
    }
}
pub(crate) fn lease(input: &Value) -> Result<Option<ReadLease>, String> {
    ReadLease::open(les_core::pdf_source::resolve(input)?)
}
fn verify(
    original: &les_core::pdf_source::Source,
    lease: Option<&ReadLease>,
) -> Result<(), String> {
    if lease.is_some_and(|guard| guard.matches(original)) {
        return Ok(());
    }
    les_core::pdf_source::verify(
        &mut File::open(&original.path).map_err(|_| "PDF_SOURCE_UNAVAILABLE")?,
        &original.sha256,
    )
    .map_err(str::to_owned)
}

pub(super) fn read(mut input: Value, lease: Option<&ReadLease>) -> Result<Value, String> {
    let original = les_core::pdf_source::resolve(&input)?;
    // A warm cache never conceals a missing/replaced original.
    verify(&original, lease)?;
    let state = input["content_root"]
        .as_str()
        .map(Path::new)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let frozen_ocr = crate::document_dispatch::configured_ocr(state.as_deref()).ok();
    let fingerprint = |path: Option<&str>| {
        path.and_then(|p| File::open(p).ok())
            .and_then(|mut file| les_core::pdf_source::fingerprint(&mut file).ok())
    };
    let derived_key = les_core::sha256_hex(les_core::tool_execution::canonical_json(&json!({
        "schema":"les.pdf-reading.v4","source":original.sha256,"page":input["source"]["page"],
        "processor":crate::document_cache::processor_revision()?,"parser":crate::document_parser::revision()?,
        "pdfium":fingerprint(std::env::var("LES_NATIVE_PDFIUM").ok().as_deref()),
        "ocr":frozen_ocr,"helper":fingerprint(frozen_ocr.as_ref().and_then(|o|o["helper"].as_str()))
    }))?.as_bytes());
    let key = json!([
        input["database"],
        original.sha256,
        input["source"]["page"],
        std::env::var_os("LES_NATIVE_DOCUMENT_PARSERS"),
        derived_key
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
        let cached = state
            .as_deref()
            .and_then(|s| crate::derived_cache::load(s, "pdf", &derived_key));
        let full = if let Some(full) =
            cached.filter(|v| v["text"].as_str().is_some_and(|s| !s.trim().is_empty()))
        {
            full
        } else {
            let full = match worker_source(input.clone()) {
                Err(code) if code == "PDF_OCR_REQUIRED" => {
                    let mut ocr = frozen_ocr.clone().ok_or("OCR_UNAVAILABLE")?;
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
            // Cache only the whole page snapshot. Query windows and cursors are
            // applied below; a cache hit still verifies the original twice.
            verify(&original, lease)?;
            if full["text"].as_str().is_some_and(|s| !s.trim().is_empty()) {
                if let Some(state) = state.as_deref() {
                    crate::derived_cache::store(state, "pdf", &derived_key, &full);
                }
            }
            full
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
    verify(&original, lease)?;
    Ok(value)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn lease_prevents_writes_and_replacement_only_until_the_request_ends() {
        let root = std::env::temp_dir().join(format!("les-pdf-lease-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("original.pdf");
        std::fs::write(&path, b"Original revision").unwrap();
        let source = || les_core::pdf_source::Source {
            path: path.canonicalize().unwrap(),
            identity: json!({}),
            sha256: les_core::sha256_hex(b"Original revision"),
        };
        let lease = ReadLease::open(source()).unwrap().unwrap();
        assert!(lease.matches(&source()));
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::rename(&path, root.join("moved.pdf")).is_err());
        let mut other = source();
        other.sha256 = "a".repeat(64);
        assert!(!lease.matches(&other));
        assert_eq!(
            verify(&other, Some(&lease)).unwrap_err(),
            "PDF_REVISION_CHANGED"
        );
        drop(lease);
        std::fs::write(&path, b"New revision").unwrap();
        assert_eq!(verify(&source(), None).unwrap_err(), "PDF_REVISION_CHANGED");
        std::fs::remove_dir_all(root).unwrap();
    }
}
