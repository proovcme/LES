//! Bounded, version-keyed derived data in owned state. Never stores an error as text.
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};
const ENTRY: u64 = 16 * 1024 * 1024;
const TOTAL: u64 = 128 * 1024 * 1024;
static ID: AtomicU64 = AtomicU64::new(0);
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn name(kind: &str, key: &str) -> Option<String> {
    (matches!(kind, "word" | "picture" | "pdf") && hash(key)).then(|| format!("{kind}-{key}.json"))
}
fn folder(state: &Path) -> Option<PathBuf> {
    let state = state.canonicalize().ok()?;
    let mut root = state.clone();
    for part in ["storage", "derived-views"] {
        root.push(part);
        if !root.exists() {
            fs::create_dir(&root).ok()?;
        }
        let meta = fs::symlink_metadata(&root).ok()?;
        if !meta.is_dir() || meta.file_type().is_symlink() || root.canonicalize().ok()? != root {
            return None;
        }
    }
    Some(root)
}
pub(crate) fn load(state: &Path, kind: &str, key: &str) -> Option<Value> {
    let path = folder(state)?.join(name(kind, key)?);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > ENTRY + 4096 {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .ok()?
        .take(ENTRY + 4097)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > ENTRY + 4096 {
        return None;
    }
    let entry: Value = serde_json::from_slice(&bytes).ok()?;
    if entry["schema"] != "les.derived-cache.v2"
        || entry["key"] != key
        || entry["kind"] != kind
        || entry["payload_sha256"]
            != les_core::sha256_hex(
                les_core::tool_execution::canonical_json(&entry["payload"])
                    .ok()?
                    .as_bytes(),
            )
    {
        return None;
    }
    Some(entry["payload"].clone())
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub(crate) fn store(state: &Path, kind: &str, key: &str, payload: &Value) {
    // Caching is optional. Permission, disk or corruption failures never turn
    // a successful source read into an empty document or bypass source checks.
    let Some(root) = folder(state) else { return };
    let Some(name) = name(kind, key) else { return };
    let Ok(body) = les_core::tool_execution::canonical_json(payload) else {
        return;
    };
    if body.len() as u64 > ENTRY {
        return;
    }
    let entry = json!({"schema":"les.derived-cache.v2","kind":kind,"key":key,
        "payload_sha256":les_core::sha256_hex(body.as_bytes()),"payload":payload})
    .to_string();
    if entry.len() as u64 > ENTRY + 4096 {
        return;
    }
    let temp = Temporary(root.join(format!(
        "write-{}-{}.tmp",
        std::process::id(),
        ID.fetch_add(1, Ordering::Relaxed)
    )));
    let publish = || -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp.0)?;
        file.write_all(entry.as_bytes())?;
        file.sync_all()?;
        drop(file);
        let target = root.join(name);
        if target.exists() {
            let meta = fs::symlink_metadata(&target)?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Ok(());
            }
            if load(state, kind, key).is_some() {
                return Ok(());
            }
            fs::remove_file(&target)?;
        }
        fs::rename(&temp.0, &target)?;
        Ok(())
    };
    if publish().is_ok() {
        sweep(&root);
    }
}
fn sweep(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    let mut published = Vec::new();
    let mut size = 0u64;
    for item in entries.take(4096).flatten() {
        let Ok(meta) = fs::symlink_metadata(item.path()) else {
            continue;
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            continue;
        }
        let Some(file) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let modified = meta.modified().unwrap_or(now);
        if file.starts_with("write-")
            && file.ends_with(".tmp")
            && file[6..file.len() - 4]
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'-')
            && now
                .duration_since(modified)
                .is_ok_and(|age| age > Duration::from_secs(3600))
        {
            let _ = fs::remove_file(item.path());
        }
        if let Some((kind, key)) = file.strip_suffix(".json").and_then(|s| s.split_once('-')) {
            if name(kind, key).as_deref() == Some(file.as_str()) {
                size = size.saturating_add(meta.len());
                published.push((modified, item.path(), meta.len()));
            }
        }
    }
    published.sort_by_key(|x| x.0);
    let mut count = published.len();
    for (_, path, bytes) in published {
        if size <= TOTAL && count <= 1024 {
            break;
        }
        if fs::remove_file(path).is_ok() {
            size = size.saturating_sub(bytes);
            count = count.saturating_sub(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_corrupt_or_wrong_revision_data_is_a_miss() {
        let root = std::env::temp_dir().join(format!("les-derived-cache-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let key = "a".repeat(64);
        let value = json!({"z":{"b":2,"a":1},"text":"Exact 17.25","a":[{"second":2,"first":1}]});
        store(&root, "word", &key, &value);
        assert_eq!(load(&root, "word", &key), Some(value.clone()));
        assert!(load(&root, "word", &"b".repeat(64)).is_none());
        let path = folder(&root).unwrap().join(name("word", &key).unwrap());
        let mut corrupt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        corrupt["payload"]["text"] = json!("117.25");
        fs::write(&path, corrupt.to_string()).unwrap();
        assert!(load(&root, "word", &key).is_none());
        store(&root, "word", &key, &value);
        assert_eq!(load(&root, "word", &key), Some(value));
        fs::write(folder(&root).unwrap().join("write-0-0.tmp"), "{partial").unwrap();
        assert!(load(&root, "picture", &key).is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
