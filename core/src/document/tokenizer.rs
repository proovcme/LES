//! Read the configured local Hugging Face tokenizer without downloading a model.
use serde_json::{json, Value};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};
use tokenizers::Tokenizer;

const LIMIT: u64 = 64 * 1024 * 1024;
const UNAVAILABLE: &str = "DOCUMENT_TOKENIZER_UNAVAILABLE";
const CHANGED: &str = "DOCUMENT_TOKENIZER_CHANGED";

pub struct Local {
    fingerprint: String,
    tokenizer: Tokenizer,
}

struct Cached {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
    local: Arc<Local>,
}

static CACHE: OnceLock<Mutex<Vec<Cached>>> = OnceLock::new();

impl Local {
    pub fn count(&self, text: &str) -> Result<usize, &'static str> {
        self.tokenizer
            .encode_fast(text, false)
            .map(|encoded| encoded.len())
            .map_err(|_| "DOCUMENT_TOKENIZER_FAILED")
    }
}

pub fn load(request: &Value) -> Result<Arc<Local>, &'static str> {
    let path = Path::new(request["path"].as_str().ok_or(UNAVAILABLE)?);
    if !path.is_absolute() {
        return Err(UNAVAILABLE);
    }
    let metadata = path.metadata().map_err(|_| UNAVAILABLE)?;
    let modified = metadata.modified().map_err(|_| UNAVAILABLE)?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(UNAVAILABLE);
    }
    let expected = request["sha256"].as_str();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| UNAVAILABLE)?;
    if let Some(hit) = cache.iter().position(|entry| {
        entry.path == path && entry.modified == modified && entry.size == metadata.len()
    }) {
        let entry = cache.remove(hit);
        let local = Arc::clone(&entry.local);
        cache.push(entry);
        if expected.is_some_and(|sha| sha != local.fingerprint) {
            return Err(CHANGED);
        }
        return Ok(local);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| UNAVAILABLE)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNAVAILABLE)?;
    let after = path.metadata().map_err(|_| CHANGED)?;
    if bytes.len() as u64 != metadata.len()
        || after.len() != metadata.len()
        || after.modified().ok() != Some(modified)
    {
        return Err(CHANGED);
    }
    let fingerprint = crate::sha256_hex(&bytes);
    if expected.is_some_and(|sha| sha != fingerprint) {
        return Err(CHANGED);
    }
    let mut tokenizer = Tokenizer::from_bytes(&bytes).map_err(|_| UNAVAILABLE)?;
    tokenizer.with_truncation(None).map_err(|_| UNAVAILABLE)?;
    tokenizer.with_padding(None);
    let local = Arc::new(Local {
        fingerprint,
        tokenizer,
    });
    cache.retain(|entry| entry.path != path);
    if cache.len() == 2 {
        cache.remove(0);
    }
    cache.push(Cached {
        path: path.to_owned(),
        modified,
        size: metadata.len(),
        local: Arc::clone(&local),
    });
    Ok(local)
}

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    let local = load(&request["tokenizer"])?;
    match request["action"].as_str() {
        Some("tokenizer_info") => Ok(json!({"sha256":local.fingerprint})),
        Some("token_count") => {
            Ok(json!({"count":local.count(request["text"].as_str().ok_or(UNAVAILABLE)?)?}))
        }
        _ => Err(UNAVAILABLE),
    }
}
