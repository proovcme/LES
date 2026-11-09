//! Opaque, expiring attachments. A display name or model argument never grants a path.
mod folder;
mod picture;
mod preview;
mod upload;
pub use upload::{claim, replay_upload, save_upload, UploadClaim};
#[cfg(test)]
mod tests;
pub use preview::convert as preview_with;
pub use preview::snapshot as table_snapshot;
pub fn serve_with(external: &impl Fn(&Value) -> Result<Value>) -> Result<()> {
    crate::document::conversion::serve_requests(&|req| {
        if req["action"] == "table_task" {
            Ok(crate::document_tasks::execute(&req["request"]))
        } else if req["action"] == "prepare" {
            preview_with(req, external)
        } else if req["action"] == "folder" {
            folder::snapshot(req, external)
        } else {
            execute(req)
        }
    })
}
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub type Result<T> = std::result::Result<T, &'static str>;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_CARD: u64 = 512 * 1024;
pub const MAX_AGE: u64 = 7 * 24 * 3600;
pub struct Asset {
    pub path: PathBuf,
    pub metadata: Value,
    pub file: File,
}
fn id(s: &str) -> Result<&str> {
    if s.len() == 17
        && s.starts_with("read_")
        && s.as_bytes()[5..]
            .iter()
            .all(|c| c.is_ascii_digit() || (*c >= b'a' && *c <= b'f'))
    {
        Ok(s)
    } else {
        Err("ATTACHMENT_ID_INVALID")
    }
}
pub fn new_id() -> Result<String> {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    Ok(format!(
        "read_{}",
        b.iter().map(|c| format!("{c:02x}")).collect::<String>()
    ))
}
pub(crate) fn regular(path: &Path) -> Result<bool> {
    let m = match fs::symlink_metadata(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("ATTACHMENT_STORE_UNAVAILABLE"),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err("ATTACHMENT_PATH_UNSAFE");
        }
    }
    if m.file_type().is_symlink() {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    Ok(true)
}
fn root(path: &Path, create: bool) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    if create {
        fs::create_dir_all(path).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    }
    if !regular(path)? {
        return Err("ATTACHMENT_NOT_FOUND");
    }
    path.canonicalize()
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn number(v: &Value) -> Result<f64> {
    let n = if let Some(s) = v.as_str() {
        s.parse::<f64>().ok()
    } else {
        v.as_f64()
    }
    .ok_or("ATTACHMENT_CARD_INVALID")?;
    if n.is_finite() && n >= 0. {
        Ok(n)
    } else {
        Err("ATTACHMENT_CARD_INVALID")
    }
}
fn read_card(path: &Path) -> Result<Value> {
    if !regular(path)? {
        return Err("ATTACHMENT_NOT_FOUND");
    }
    let f = File::open(path).map_err(|_| "ATTACHMENT_NOT_FOUND")?;
    if !f
        .metadata()
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?
        .is_file()
    {
        return Err("ATTACHMENT_CARD_INVALID");
    }
    let mut bytes = Vec::new();
    f.take(MAX_CARD + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    if bytes.len() as u64 > MAX_CARD {
        return Err("ATTACHMENT_CARD_INVALID");
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or("ATTACHMENT_CARD_INVALID")
}
fn source(base: &Path, ident: &str, card: &Value) -> Result<PathBuf> {
    if card["attachment_id"] != ident {
        return Err("ATTACHMENT_CARD_INVALID");
    }
    let name = card["stored_name"]
        .as_str()
        .ok_or("ATTACHMENT_CARD_INVALID")?;
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        || !(Path::new(name).file_stem().and_then(|s| s.to_str()) == Some(ident)
            || name
                .strip_prefix(&format!("{ident}.source."))
                .is_some_and(|s| {
                    !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_alphanumeric())
                }))
    {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    let path = base.join(name);
    if !regular(&path)? {
        return Err("ATTACHMENT_NOT_FOUND");
    }
    if path
        .canonicalize()
        .map_err(|_| "ATTACHMENT_NOT_FOUND")?
        .parent()
        != Some(base)
    {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    Ok(path)
}
pub(crate) fn digest(file: &mut File) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    let mut size = 0;
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| "ATTACHMENT_READ_FAILED")?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_FILE {
            return Err("ATTACHMENT_FILE_LIMIT");
        }
        hash.update(&bytes[..n]);
    }
    Ok((size, format!("{:x}", hash.finalize())))
}
pub fn resolve(base: &Path, ident: &str, age: u64) -> Result<Asset> {
    resolve_for(base, ident, age, None)
}
pub fn resolve_for(base: &Path, ident: &str, age: u64, caller: Option<&str>) -> Result<Asset> {
    use std::io::{Seek, SeekFrom};
    let ident = id(ident)?;
    let base = root(base, false)?;
    let card = read_card(&base.join(format!("{ident}.json")))?;
    if let (Some(owner), Some(caller)) = (card["upload_owner"].as_str(), caller) {
        if owner != caller {
            return Err("ATTACHMENT_NOT_FOUND");
        }
    }
    if card["upload_expired"] == true {
        return Err("ATTACHMENT_EXPIRED");
    }
    let created = number(&card["created_at_epoch"])?;
    if created <= 0. || now() - created > age as f64 {
        consume(&base, ident)?;
        return Err("ATTACHMENT_EXPIRED");
    }
    let path = source(&base, ident, &card)?;
    let mut file = File::open(&path).map_err(|_| "ATTACHMENT_NOT_FOUND")?;
    let m = file.metadata().map_err(|_| "ATTACHMENT_READ_FAILED")?;
    if !m.is_file() || m.len() as f64 != number(&card["size"])? {
        return Err("ATTACHMENT_SIZE_CHANGED");
    }
    let (size, sha) = digest(&mut file)?;
    if size != m.len() || Some(sha.as_str()) != card["sha256"].as_str() {
        return Err("ATTACHMENT_HASH_CHANGED");
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "ATTACHMENT_READ_FAILED")?;
    Ok(Asset {
        path,
        metadata: card,
        file,
    })
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn temporary(base: &Path, ident: &str) -> Result<(Temporary, File)> {
    let path = base.join(format!(".{ident}.{}.tmp", new_id()?));
    let f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    Ok((Temporary(path), f))
}
fn lease(base: &Path, ident: &str) -> Result<File> {
    let path = base.join(format!("{ident}.lock"));
    regular(&path)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    f.try_lock().map_err(|_| "ATTACHMENT_BUSY")?;
    Ok(f)
}
pub fn preserve(base: &Path, path: &Path, ident: &str, name: &str) -> Result<Value> {
    preserve_inner(base, path, ident, name, None)
}
fn preserve_inner(
    base: &Path,
    path: &Path,
    ident: &str,
    name: &str,
    upload: Option<(&UploadClaim, &Value)>,
) -> Result<Value> {
    let ident = id(ident)?;
    let base = root(base, true)?;
    let _lease = lease(&base, ident)?;
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty() && s.chars().count() <= 1000 && !s.chars().any(char::is_control))
        .ok_or("ATTACHMENT_NAME_INVALID")?;
    let suffix = Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if suffix.len() > 12 || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("ATTACHMENT_NAME_INVALID");
    }
    let stored = format!(
        "{ident}.source{}{}",
        if suffix.is_empty() { "" } else { "." },
        suffix
    );
    let target = base.join(&stored);
    let metadata_path = base.join(format!("{ident}.json"));
    if regular(&metadata_path)? {
        if let Some((claim, _)) = upload {
            if let Some(card) = upload::existing(&base, ident, claim)? {
                return Ok(card);
            }
        }
        return Err("ATTACHMENT_ID_CONFLICT");
    }
    if regular(&target)? {
        // Only an exact key-derived path without a published card can be
        // reclaimed. The same lease guards this rename in every writer.
        if upload.is_none_or(|(claim, _)| claim.key.is_empty()) || !target.is_file() {
            return Err("ATTACHMENT_ID_CONFLICT");
        }
        fs::remove_file(&target).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    }
    let mut original = File::open(path).map_err(|_| "ATTACHMENT_READ_FAILED")?;
    if !original
        .metadata()
        .map_err(|_| "ATTACHMENT_READ_FAILED")?
        .is_file()
    {
        return Err("ATTACHMENT_READ_FAILED");
    }
    let (temp, mut output) = temporary(&base, ident)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut size = 0;
    loop {
        let count = original
            .read(&mut buffer)
            .map_err(|_| "ATTACHMENT_READ_FAILED")?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > MAX_FILE {
            return Err("ATTACHMENT_FILE_LIMIT");
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    }
    output
        .sync_all()
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    drop(output);
    let digest = format!("{:x}", hash.finalize());
    let mut card = json!({"schema":"chat_read_attachment_v1","attachment_id":ident,"original_name":name,"stored_name":stored,"suffix":if suffix.is_empty(){String::new()}else{format!(".{suffix}")},"size":size,"sha256":digest,"created_at_epoch":now()});
    if let Some((claim, response)) = upload {
        if claim.sha256 != digest || claim.size != size {
            return Err("ATTACHMENT_HASH_CHANGED");
        }
        card["upload_owner"] = json!(claim.owner);
        card["upload_key"] = json!(claim.key);
        card["upload_fingerprint"] = json!(claim.fingerprint);
        card["upload_response"] = response.clone();
        if card.to_string().len() > MAX_CARD as usize {
            return Err("ATTACHMENT_CARD_INVALID");
        }
    }
    let (tmpcard, mut file) = temporary(&base, ident)?;
    file.write_all(card.to_string().as_bytes())
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    file.sync_all()
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    drop(file);
    fs::rename(&temp.0, &target).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
    if fs::rename(&tmpcard.0, &metadata_path).is_err() {
        let _ = fs::remove_file(&target);
        return Err("ATTACHMENT_STORE_UNAVAILABLE");
    }
    Ok(card)
}
pub fn consume(base: &Path, ident: &str) -> Result<()> {
    let ident = id(ident)?;
    let base = match root(base, false) {
        Err("ATTACHMENT_NOT_FOUND") => return Ok(()),
        v => v?,
    };
    let _lease = lease(&base, ident)?;
    let card_path = base.join(format!("{ident}.json"));
    if let Ok(mut card) = read_card(&card_path) {
        if card["upload_expired"] == true {
            return Ok(());
        }
        if let Ok(path) = source(&base, ident, &card) {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("ATTACHMENT_STORE_UNAVAILABLE"),
            };
        }
        if card["upload_key"]
            .as_str()
            .is_some_and(|key| !key.is_empty())
        {
            // Keep only the reservation. Reusing a published ID with new
            // bytes could make an old draft open a different original.
            card = json!({"schema":"chat_read_attachment_v1","attachment_id":ident,
                "upload_key":card["upload_key"],"upload_owner":card["upload_owner"],
                "upload_fingerprint":card["upload_fingerprint"],"upload_expired":true});
            let (temp, mut file) = temporary(&base, ident)?;
            file.write_all(card.to_string().as_bytes())
                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
            file.sync_all()
                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
            drop(file);
            return fs::rename(&temp.0, &card_path).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE");
        }
    }
    match fs::remove_file(card_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("ATTACHMENT_STORE_UNAVAILABLE"),
    }
}
pub fn cleanup(base: &Path, age: u64) -> Result<usize> {
    cleanup_cards(base, age, 10000)
}
fn cleanup_cards(base: &Path, age: u64, limit: usize) -> Result<usize> {
    let base = match root(base, false) {
        Err("ATTACHMENT_NOT_FOUND") => return Ok(0),
        v => v?,
    };
    let mut removed = 0;
    let mut cards = 0;
    for entry in fs::read_dir(&base).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")? {
        let path = entry.map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?.path();
        let Some(ident) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| id(s).is_ok())
        else {
            continue;
        };
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(card) = read_card(&path) {
            if card["upload_expired"] == true {
                continue;
            }
            cards += 1;
            if cards > limit {
                break;
            }
            if number(&card["created_at_epoch"]).is_ok_and(|n| n <= 0. || now() - n > age as f64) {
                consume(&base, ident)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}
pub fn execute(req: &Value) -> Result<Value> {
    if req["schema"] != "les.chat-attachments.v1" {
        return Err("ATTACHMENT_SCHEMA_INVALID");
    }
    let base = Path::new(req["root"].as_str().ok_or("ATTACHMENT_PATH_UNSAFE")?);
    let ident = req["attachment_id"].as_str().unwrap_or("");
    let age = req["max_age_sec"].as_u64().unwrap_or(MAX_AGE);
    match req["action"].as_str() {
        Some("preserve") => preserve(
            base,
            Path::new(req["source"].as_str().ok_or("ATTACHMENT_PATH_UNSAFE")?),
            ident,
            req["original_name"]
                .as_str()
                .ok_or("ATTACHMENT_NAME_INVALID")?,
        ),
        Some("resolve") => {
            let asset = resolve(base, ident, age)?;
            Ok(json!({"path":asset.path,"metadata":asset.metadata}))
        }
        Some("consume") => {
            consume(base, ident)?;
            Ok(Value::Null)
        }
        Some("cleanup") => Ok(json!(cleanup(base, age)?)),
        Some("image") => picture::file(Path::new(
            req["path"].as_str().ok_or("ATTACHMENT_PATH_UNSAFE")?,
        )),
        Some("resolve_image") => {
            picture::asset(resolve_for(base, ident, age, req["upload_owner"].as_str())?)
        }
        Some("preview") => preview::execute(req),
        _ => Err("ATTACHMENT_ACTION_INVALID"),
    }
}
