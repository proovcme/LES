//! Read-only fingerprints; the caller retains source scope and index ownership.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read};

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(windows)]
fn identity(file: &File) -> Result<(u64, u64), &'static str> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut u32) -> i32;
    }
    let mut info = [0_u32; 13]; // BY_HANDLE_FILE_INFORMATION: thirteen DWORDs.
                                // SAFETY: a live File supplies the handle and the array is the exact ABI layout.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err("CORE_FILE_UNAVAILABLE");
    }
    Ok((info[7] as u64, ((info[11] as u64) << 32) | info[12] as u64))
}

#[cfg(unix)]
fn identity(file: &File) -> Result<(u64, u64), &'static str> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(|_| "CORE_FILE_UNAVAILABLE")?;
    Ok((metadata.dev(), metadata.ino()))
}

pub fn fingerprint(request: &Value) -> Result<Value, &'static str> {
    let path = request["path"].as_str().ok_or("CORE_FILE_PATH_INVALID")?;
    fingerprint_path(path, || {})
}

fn fingerprint_path(path: &str, after_read: impl FnOnce()) -> Result<Value, &'static str> {
    if !std::fs::metadata(path)
        .map_err(|_| "CORE_FILE_UNAVAILABLE")?
        .is_file()
    {
        return Err("CORE_NOT_REGULAR_FILE");
    }
    let mut file = File::open(path).map_err(|_| "CORE_FILE_UNAVAILABLE")?;
    let before = file.metadata().map_err(|_| "CORE_FILE_UNAVAILABLE")?;
    if !before.is_file() {
        return Err("CORE_NOT_REGULAR_FILE");
    }
    let id = identity(&file)?;
    let modified = before.modified().map_err(|_| "CORE_FILE_UNAVAILABLE")?;
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    loop {
        let length = file
            .read(&mut buffer)
            .map_err(|_| "CORE_FILE_UNAVAILABLE")?;
        if length == 0 {
            break;
        }
        size += length as u64;
        if size > before.len() {
            return Err("CORE_FILE_CHANGED");
        }
        hash.update(&buffer[..length]);
    }
    after_read();
    let after = file.metadata().map_err(|_| "CORE_FILE_UNAVAILABLE")?;
    let current = File::open(path).map_err(|_| "CORE_FILE_CHANGED")?;
    let current_meta = current.metadata().map_err(|_| "CORE_FILE_CHANGED")?;
    if size != before.len()
        || size != after.len()
        || size != current_meta.len()
        || after.modified().ok() != Some(modified)
        || current_meta.modified().ok() != Some(modified)
        || identity(&current)? != id
    {
        return Err("CORE_FILE_CHANGED");
    }
    Ok(json!({"sha256":format!("{:x}",hash.finalize()), "size":size, "stable":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_sources_are_not_empty_fingerprints() {
        assert!(fingerprint(&json!({"path":"\0invalid"})).is_err());
    }
    #[test]
    fn a_source_changed_after_read_is_rejected() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/file-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("source.txt");
        std::fs::write(&path, b"original").unwrap();
        let result = fingerprint_path(path.to_str().unwrap(), || {
            std::fs::write(&path, b"changed size").unwrap();
        });
        assert_eq!(result, Err("CORE_FILE_CHANGED"));
    }
}
