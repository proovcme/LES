//! Model credentials stay behind an allowlisted local store; diagnostics never include their value.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

fn unavailable(stage: &'static str, cause: impl std::fmt::Display) -> &'static str {
    eprintln!(
        "{}",
        json!({"event":"model_settings_io_failed","stage":stage,"cause":cause.to_string()})
    );
    "SECRET_STORE_UNAVAILABLE"
}

fn publish(temporary: &Path, path: &Path, expected: &str) -> Result<(), &'static str> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        if read(path)? != expected {
            return Err("SECRET_STORE_CHANGED");
        }
        match fs::rename(temporary, path) {
            Ok(()) => return Ok(()),
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(5 | 32 | 33))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(unavailable("atomic_publish", error)),
        }
    }
}

pub fn key(reference: &str) -> Result<&str, &'static str> {
    let key = reference
        .trim()
        .strip_prefix("env:")
        .ok_or("SECRET_REF_NOT_ALLOWED")?;
    let migration = [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "OLLAMA_API_KEY",
        "LEMONADE_API_KEY",
        "FREETOKEN_API_KEY",
    ]
    .contains(&key);
    let connection = key
        .strip_prefix("LES_MODEL_CONNECTION_")
        .and_then(|v| v.strip_suffix("_API_KEY"))
        .is_some_and(|v| {
            !v.is_empty()
                && v.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        });
    if migration || connection {
        Ok(key)
    } else {
        Err("SECRET_REF_NOT_ALLOWED")
    }
}
fn read(path: &Path) -> Result<String, &'static str> {
    let mut text = String::new();
    match File::open(path) {
        Ok(file) => {
            file.take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|error| unavailable("read_text", error))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable("read_open", error)),
    }
    if text.len() > 8 * 1024 * 1024 {
        Err("SECRET_STORE_LIMIT")
    } else {
        Ok(text)
    }
}
pub fn file_values(path: &Path) -> Result<BTreeMap<String, String>, &'static str> {
    values(path, true)
}
/// Configuration follows dotenv's last-definition-wins ordering. Secret reads
/// retain their established first-match contract; rotating a secret removes duplicates.
pub fn settings_values(path: &Path) -> Result<BTreeMap<String, String>, &'static str> {
    values(path, false)
}
fn values(path: &Path, first: bool) -> Result<BTreeMap<String, String>, &'static str> {
    let mut values = BTreeMap::new();
    for line in read(path)?.lines() {
        if let Some((name, value)) = line.split_once('=') {
            if first {
                values
                    .entry(name.trim().into())
                    .or_insert_with(|| value.trim().into());
            } else {
                values.insert(name.trim().into(), value.trim().into());
            }
        }
    }
    Ok(values)
}
pub fn resolve(
    path: &Path,
    reference: Option<&str>,
    environment: impl FnOnce(&str) -> Option<String>,
) -> Result<Option<String>, &'static str> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    let key = key(reference)?;
    if let Some(value) = environment(key).filter(|v| !v.trim().is_empty()) {
        return Ok(Some(value.trim().into()));
    }
    file_values(path)?
        .remove(key)
        .filter(|v| !v.is_empty())
        .map(Some)
        .ok_or("SECRET_MISSING")
}
pub fn replace(
    path: &Path,
    reference: &str,
    value: &str,
    actor: &str,
) -> Result<Value, &'static str> {
    let key = key(reference)?;
    if value.trim().is_empty() || value.contains(['\n', '\r']) || value.chars().count() > 4096 {
        return Err("SECRET_VALUE_INVALID");
    }
    let actor = actor.trim();
    if actor.is_empty() {
        return Err("SECRET_ACTOR_REQUIRED");
    }
    if !path.is_absolute() {
        return Err("SECRET_STORE_PATH_INVALID");
    }
    let parent = path.parent().ok_or("SECRET_STORE_PATH_INVALID")?;
    fs::create_dir_all(parent).map_err(|error| unavailable("create_directory", error))?;
    let file_name = path
        .file_name()
        .ok_or("SECRET_STORE_PATH_INVALID")?
        .to_string_lossy();
    let lock_path = parent.join(format!("{file_name}.lock"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| unavailable("lock_open", error))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(std::fs::TryLockError::WouldBlock) => return Err("SECRET_STORE_BUSY"),
            Err(error) => return Err(unavailable("lock_acquire", error)),
        }
    }
    let existing = read(path)?;
    let mut lines = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if line
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == key)
        {
            if !replaced {
                lines.push(format!("{key}={value}"));
                replaced = true;
            }
        } else {
            lines.push(line.to_owned());
        }
    }
    if !replaced {
        lines.push(format!("{key}={value}"));
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "SECRET_STORE_UNAVAILABLE")?;
    let token = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let temporary = parent.join(format!("{file_name}.{token}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options
        .open(&temporary)
        .map_err(|error| unavailable("temporary_create", error))?;
    let result: Result<(), &'static str> = (|| {
        #[cfg(windows)]
        copy_dacl(path, &temporary)?;
        output
            .write_all((lines.join("\n") + "\n").as_bytes())
            .and_then(|_| output.sync_all())
            .map_err(|error| unavailable("temporary_sync", error))?;
        drop(output);
        publish(&temporary, path, &existing)?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| "SECRET_STORE_UNAVAILABLE")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(
        json!({"secret_ref":format!("env:{key}"),"status":"configured","actor":actor,
        "updated_at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,false)}),
    )
}
#[cfg(windows)]
fn copy_dacl(source: &Path, target: &Path) -> Result<(), &'static str> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};
    if !source
        .try_exists()
        .map_err(|_| "SECRET_STORE_UNAVAILABLE")?
    {
        return Ok(());
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn GetNamedSecurityInfoW(
            name: *const u16,
            kind: u32,
            info: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn SetNamedSecurityInfoW(
            name: *mut u16,
            kind: u32,
            info: u32,
            owner: *mut c_void,
            group: *mut c_void,
            dacl: *mut c_void,
            sacl: *mut c_void,
        ) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    // File names are NUL-terminated; Windows owns the returned descriptor until LocalFree.
    let read = unsafe {
        GetNamedSecurityInfoW(
            source.as_ptr(),
            1,
            4,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if read != 0 {
        return Err(unavailable(
            "permissions_read",
            std::io::Error::from_raw_os_error(read as i32),
        ));
    }
    let write = unsafe {
        SetNamedSecurityInfoW(
            target.as_mut_ptr(),
            1,
            4 | 0x80000000,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        )
    };
    unsafe { LocalFree(descriptor) };
    if write == 0 {
        Ok(())
    } else {
        Err(unavailable(
            "permissions_write",
            std::io::Error::from_raw_os_error(write as i32),
        ))
    }
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    if request["schema"] != "les.model-secret.v1" {
        return Err("SECRET_SCHEMA_INVALID");
    }
    if request["action"] == "key" {
        return Ok(
            json!({"key":key(request["secret_ref"].as_str().ok_or("SECRET_REF_NOT_ALLOWED")?)?}),
        );
    }
    let path = Path::new(
        request["env_path"]
            .as_str()
            .ok_or("SECRET_STORE_PATH_INVALID")?,
    );
    if !path.is_absolute() {
        return Err("SECRET_STORE_PATH_INVALID");
    }
    let reference = request["secret_ref"].as_str();
    match request["action"].as_str() {
        Some("resolve" | "status") => {
            let resolved = resolve(path, reference, |_| {
                request["environment_value"].as_str().map(str::to_owned)
            });
            if request["action"] == "status" {
                let status = match resolved {
                    Ok(None) => "not_required",
                    Ok(Some(_)) => "configured",
                    Err("SECRET_MISSING") => "missing",
                    Err(e) => return Err(e),
                };
                Ok(json!({"status":status}))
            } else {
                Ok(json!({"value":resolved?}))
            }
        }
        Some("replace") => replace(
            path,
            reference.ok_or("SECRET_REF_NOT_ALLOWED")?,
            request["value"].as_str().ok_or("SECRET_VALUE_INVALID")?,
            request["actor"].as_str().ok_or("SECRET_ACTOR_REQUIRED")?,
        ),
        _ => Err("SECRET_ACTION_INVALID"),
    }
}

#[cfg(all(test, windows))]
mod publish_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    #[test]
    fn temporary_windows_readers_delay_atomic_save_without_losing_external_changes() {
        for external_change in [false, true] {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("les-settings-share-{}-{nonce}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let path = root.join("fixture.env");
            fs::write(&path, "OTHER=unchanged\n").unwrap();
            // A short-lived scanner/editor permits reading but denies renaming.
            let reader = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(&path)
                .unwrap();
            let target = path.clone();
            let writer = std::thread::spawn(move || {
                replace(&target, "env:OPENAI_API_KEY", "fixture-value", "fixture")
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !writer.is_finished()
                && !fs::read_dir(&root)
                    .unwrap()
                    .any(|p| p.unwrap().path().extension().is_some_and(|e| e == "tmp"))
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            assert!(
                !writer.is_finished(),
                "A transient read handle must not immediately fail publication"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "OTHER=unchanged\n");
            if external_change {
                fs::write(&path, "OTHER=external-change\n").unwrap();
                // Keep rename denied until the writer has observed the change;
                // releasing it immediately would create an unrelated foreign-
                // writer compare/rename race outside the shared-lock protocol.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while !writer.is_finished() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert!(writer.is_finished());
            }
            drop(reader);
            let result = writer.join().unwrap();
            if external_change {
                assert_eq!(result.unwrap_err(), "SECRET_STORE_CHANGED");
                assert_eq!(
                    fs::read_to_string(&path).unwrap(),
                    "OTHER=external-change\n"
                );
            } else {
                assert!(result.is_ok());
                assert_eq!(file_values(&path).unwrap()["OTHER"], "unchanged");
                assert_eq!(
                    file_values(&path).unwrap()["OPENAI_API_KEY"],
                    "fixture-value"
                );
            }
            assert!(!fs::read_dir(&root).unwrap().any(|p| p
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "tmp")));
            fs::remove_file(&path).unwrap();
            fs::remove_file(root.join("fixture.env.lock")).unwrap();
            fs::remove_dir(root).unwrap();
        }
    }
}
