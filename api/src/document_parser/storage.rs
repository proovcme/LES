//! Bounded owned cache and temporary snapshots.
use super::*;
pub(super) fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
    if bytes.len() as u64 > limit {
        return Err("DOCUMENT_TEXT_LIMIT");
    }
    Ok(bytes)
}
pub(super) fn state(input: Option<&Value>) -> Result<PathBuf> {
    let root = input
        .and_then(|v| v["owned_state"].as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("LES_STATE_ROOT").map(PathBuf::from))
        .ok_or("DOCUMENT_PARSER_CACHE_INVALID")?;
    if !root.is_absolute() {
        return Err("DOCUMENT_PARSER_CACHE_INVALID");
    }
    root.canonicalize()
        .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")
}
pub(super) fn folder(root: &Path, name: &str) -> Result<PathBuf> {
    let mut path = root.to_owned();
    for component in Path::new(name).components() {
        let std::path::Component::Normal(part) = component else {
            return Err("DOCUMENT_PARSER_CACHE_INVALID");
        };
        path.push(part);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err("DOCUMENT_PARSER_CACHE_INVALID");
            }
        } else {
            fs::create_dir(&path).map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
        }
        if path
            .canonicalize()
            .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?
            != path
        {
            return Err("DOCUMENT_PARSER_CACHE_INVALID");
        }
    }
    housekeeping(&path, name);
    Ok(path)
}
fn housekeeping(root: &Path, kind: &str) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let clock = if kind.ends_with("parser-tmp") {
        &TEMP_CLEAN
    } else {
        &TABLE_CLEAN
    };
    let previous = clock.load(Ordering::Relaxed);
    if stamp.saturating_sub(previous) < 60
        || clock
            .compare_exchange(previous, stamp, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let mut cached = Vec::new();
    let mut size = 0_u64;
    for entry in entries.take(8192).flatten() {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let modified = meta.modified().unwrap_or(now);
        if kind.ends_with("parser-tmp")
            && name
                .strip_prefix("parser-")
                .and_then(|s| s.strip_suffix(".tmp"))
                .is_some_and(|s| s.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
            && now
                .duration_since(modified)
                .is_ok_and(|age| age.as_secs() > 86400)
        {
            let _ = fs::remove_file(entry.path());
        }
        let parts = name
            .strip_suffix(".json")
            .map(|s| s.split('-').collect::<Vec<_>>())
            .unwrap_or_default();
        if kind.ends_with("parser-tables")
            && (parts.len() == 2
                || parts.len() == 3
                    && parts[2]
                        .strip_prefix('p')
                        .is_some_and(|s| s.parse::<u64>().is_ok_and(|p| (1..=10000).contains(&p))))
            && parts[..2].iter().all(|s| valid_hash(s))
        {
            size = size.saturating_add(meta.len());
            cached.push((modified, entry.path(), meta.len()));
        }
    }
    cached.sort_by_key(|v| v.0);
    for (_, path, length) in cached {
        if size <= 256 * 1024 * 1024 {
            break;
        }
        if fs::remove_file(path).is_ok() {
            size = size.saturating_sub(length);
        }
    }
}
pub(super) struct Temporary(pub(super) PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub(super) fn temporary(root: &Path, bytes: &[u8]) -> Result<Temporary> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?
        .as_nanos();
    let name = format!(
        "parser-{}-{stamp}-{}.tmp",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    );
    let path = root.join(name);
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
    let own = Temporary(path);
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|_| "DOCUMENT_PARSER_CACHE_INVALID")?;
    Ok(own)
}
