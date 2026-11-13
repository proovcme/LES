//! Delete only old OCR bitmaps whose producing Windows process is confirmed dead.
use std::{path::Path, time::Duration};

pub(crate) fn cleanup(state: &Path) -> usize {
    let Ok(state) = state.canonicalize() else {
        return 0;
    };
    let expected = state.join("storage/ocr-tmp");
    let Ok(root) = expected.canonicalize() else {
        return 0;
    };
    if root != expected {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((pid, nonce)) = name
            .strip_prefix("page-")
            .and_then(|s| s.strip_suffix(".bmp"))
            .and_then(|s| s.split_once('-'))
        else {
            continue;
        };
        let Some(pid) = pid.parse::<u32>().ok().filter(|p| *p > 0) else {
            continue;
        };
        if nonce.is_empty() || !nonce.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        if age.is_none_or(|age| age < Duration::from_secs(60)) || !dead(pid) {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        eprintln!(
            "{}",
            serde_json::json!({"event":"ocr_scratch_recovered","removed":removed})
        );
    }
    removed
}

#[cfg(windows)]
fn dead(pid: u32) -> bool {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetExitCodeProcess(handle: *mut c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }
    unsafe {
        let handle = OpenProcess(0x1000, 0, pid); // PROCESS_QUERY_LIMITED_INFORMATION
        if handle.is_null() {
            return GetLastError() == 87;
        } // Invalid PID; access denied is unknown.
        let mut code = 259;
        let known = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        known && code != 259
    }
}
#[cfg(not(windows))]
fn dead(_pid: u32) -> bool {
    false
}
