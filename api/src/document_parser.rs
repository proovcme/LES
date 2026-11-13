//! Isolated parser asset, bounded temporary snapshots and revision-bound PDF table cache.
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
};
type Result<T> = std::result::Result<T, &'static str>;
#[derive(Clone)]
struct Asset {
    path: PathBuf,
    hash: String,
}
static ASSET: OnceLock<Result<Option<Asset>>> = OnceLock::new();
static TEMP_ID: AtomicU64 = AtomicU64::new(0);
static TEMP_CLEAN: AtomicU64 = AtomicU64::new(0);
static TABLE_CLEAN: AtomicU64 = AtomicU64::new(0);
fn asset() -> Result<Option<Asset>> {
    ASSET
        .get_or_init(|| {
            let explicit = std::env::var_os("LES_NATIVE_DOCUMENT_PARSERS");
            let path = explicit.clone().map(PathBuf::from).or_else(|| {
                std::env::var_os("LES_RUNTIME_HOME")
                    .map(PathBuf::from)
                    .map(|p| {
                        p.join("native/parsers").join(if cfg!(windows) {
                            "les-document-parsers.exe"
                        } else {
                            "les-document-parsers"
                        })
                    })
            });
            let Some(path) = path else { return Ok(None) };
            if !path.exists() && explicit.is_none() {
                return Ok(None);
            }
            if !path.is_absolute() || !path.is_file() {
                return Err("DOCUMENT_PARSER_UNAVAILABLE");
            }
            let receipt: Value = serde_json::from_slice(&bounded(
                &path.with_file_name("parser-receipt.json"),
                65536,
            )?)
            .map_err(|_| "DOCUMENT_PARSER_ASSET_MISMATCH")?;
            let hash = les_core::sha256_hex(&bounded(&path, 256 * 1024 * 1024)?);
            if receipt["schema"] != "les.document-parsers-asset.v1"
                || receipt["revision"] != "les.parsers.v1"
                || receipt["file"].as_str() != path.file_name().and_then(|s| s.to_str())
                || receipt["sha256"].as_str() != Some(hash.as_str())
            {
                return Err("DOCUMENT_PARSER_ASSET_MISMATCH");
            }
            Ok(Some(Asset { path, hash }))
        })
        .clone()
}
pub(crate) fn enabled() -> Result<bool> {
    Ok(asset()?.is_some())
}
fn invoke(input: Value) -> Result<Value> {
    let asset = asset()?.ok_or("DOCUMENT_PARSER_UNAVAILABLE")?;
    let reply = les_core::run_native_parsers(
        &json!({"schema":"les.worker-host.v1","program":asset.path,
        "directory":asset.path.parent(),"args":[],"context":asset.hash,"timeout_ms":120000,"input":input}),
    )?;
    if reply["status"] == "completed" && reply["result"]["status"] == "completed" {
        return Ok(reply["result"]["value"].clone());
    }
    let code = reply["result"]["code"]
        .as_str()
        .or_else(|| reply["code"].as_str())
        .unwrap_or("");
    let known = [
        "NO_SEARCHABLE_TEXT",
        "DOCLING_WORD_INCOMPLETE",
        "DOCLING_WORD_FAILED",
        "DOCUMENT_TEXT_LIMIT",
        "DOCUMENT_ARCHIVE_INVALID",
        "DOCUMENT_ARCHIVE_LIMIT",
        "SOURCE_CHANGED_DURING_INDEXING",
        "XBERG_PDF_TABLES_INVALID",
        "XBERG_PDF_TABLES_FAILED",
        "WORKER_TIMEOUT",
        "WORKER_EXITED",
        "WORKER_OUTPUT_LIMIT",
        "MEMORY_ADMISSION_BLOCKED",
    ];
    Err(known
        .into_iter()
        .find(|v| *v == code)
        .unwrap_or("DOCUMENT_PARSER_FAILED"))
}

mod pdf;
mod storage;
mod word;
use pdf::valid_hash;
pub(crate) use pdf::{page_view, pdf_page_tables, pdf_tables};
use storage::{bounded, folder, state, temporary};
pub(crate) use word::{legacy_word, word};
