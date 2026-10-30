//! Versioned C ABI. All returned buffers belong to this library until les_core_free.
mod files;
mod jobs;
mod lexical;
use serde_json::{json, Value};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
};

pub const MAX_INPUT: usize = 8 * 1024 * 1024;

fn dispatch(bytes: &[u8]) -> Result<Value, &'static str> {
    let request: Value = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
    if request["schema"] != "les.core.v1" {
        return Err("CORE_SCHEMA_MISMATCH");
    }
    match request["operation"].as_str() {
        Some("prepare_terms") => lexical::prepare(&request),
        Some("fingerprint") => files::fingerprint(&request),
        Some("job_patch") => jobs::patch(&request),
        Some("job_recovery") => jobs::recovery(&request),
        Some("health") => Ok(
            json!({"abi": 1, "operations":["prepare_terms","fingerprint","job_patch","job_recovery"], "tokenizer":"les.lexical.v2"}),
        ),
        _ => Err("CORE_UNKNOWN_OPERATION"),
    }
}

#[no_mangle]
pub extern "C" fn les_core_abi_version() -> u32 {
    1
}

/// # Safety
/// input must describe input_len readable bytes; output_len must be writable.
/// The returned non-null pointer must be freed once with its exact output length.
#[no_mangle]
pub unsafe extern "C" fn les_core_call(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
) -> *mut u8 {
    if output_len.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        *output_len = 0;
    }
    if input.is_null() || input_len > MAX_INPUT {
        return ptr::null_mut();
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let bytes = unsafe { slice::from_raw_parts(input, input_len) };
        match dispatch(bytes) {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(code) => json!({"ok":false,"code":code}),
        }
    }))
    .unwrap_or_else(|_| json!({"ok":false,"code":"CORE_PANIC"}));
    let bytes = match serde_json::to_vec(&outcome) {
        Ok(bytes) => bytes.into_boxed_slice(),
        Err(_) => return ptr::null_mut(),
    };
    unsafe {
        *output_len = bytes.len();
    }
    Box::into_raw(bytes) as *mut u8
}

/// # Safety
/// pointer and length must be exactly one live buffer returned by les_core_call.
#[no_mangle]
pub unsafe extern "C" fn les_core_free(pointer: *mut u8, length: usize) {
    if !pointer.is_null() {
        unsafe {
            drop(Box::from_raw(ptr::slice_from_raw_parts_mut(
                pointer, length,
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ffi_owns_utf8_buffers_and_rejects_wrong_schema() {
        for input in [br#"{"schema":"other"}"#.as_slice(), b"not-json", b"\xff"] {
            let mut length = 0;
            let pointer = unsafe { les_core_call(input.as_ptr(), input.len(), &mut length) };
            assert!(!pointer.is_null());
            let response: Value =
                serde_json::from_slice(unsafe { slice::from_raw_parts(pointer, length) }).unwrap();
            assert_eq!(response["ok"], false);
            unsafe {
                les_core_free(pointer, length);
            }
        }
    }
    #[test]
    fn ffi_limits_do_not_dereference_invalid_input() {
        let mut length = 9;
        assert!(unsafe { les_core_call(ptr::null(), 0, &mut length) }.is_null());
        assert_eq!(length, 0);
        assert!(unsafe { les_core_call(ptr::dangling(), MAX_INPUT + 1, &mut length) }.is_null());
    }
}
