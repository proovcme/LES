//! Versioned C ABI. All returned buffers belong to this library until les_core_free.
pub mod access_policy;
pub mod agent_files;
pub mod agent_loop;
pub mod auth;
pub mod auth_keys;
pub mod bm25;
pub mod catalog;
pub mod chat_commands;
pub mod chat_context;
pub mod chat_profiles;
pub mod chat_request;
pub mod chat_stream;
pub mod citations;
mod context;
pub mod conversation_summary;
mod document;
pub mod document_source;
pub mod embedding_native;
mod files;
pub mod filesystem;
mod harness;
pub mod harness_evidence;
pub mod index_batch;
pub mod index_bootstrap;
pub mod index_cache;
pub mod index_configuration;
mod index_dispatch;
mod index_events;
pub mod index_journal;
mod index_points;
pub mod index_publication;
mod index_sources;
mod jobs;
mod legacy_sparse;
pub mod lexical;
pub mod lexical_schema;
mod lexical_write;
pub mod mcp_registry;
mod memory;
pub mod model_capabilities;
pub mod model_profile;
pub mod model_request;
pub mod model_secrets;
pub mod pdf_source;
mod process_guard;
mod receipts;
mod retrieval_quality;
pub mod skills;
mod source_scan;
pub mod system_memory;
pub mod tables;
pub mod tool_execution;
pub mod tool_registry;
pub mod trust;
pub mod unicode;
pub mod web;
pub mod workbook_calculation;
mod worker_host;
pub mod workspace;
pub use document::conversion::serve as serve_document_formats;
pub use document::conversion::{
    execute_with as convert_document_with, serve_with as serve_document_formats_with,
};
pub use files::sha256_hex;
pub use index_sources::plan as plan_index_sources;
pub fn supports_native_document_dispatch(path: &std::path::Path) -> bool {
    document::conversion::supports(path)
}
pub use process_guard::{prepare as prepare_child, JobGuard};
use serde_json::{json, Value};
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
};
pub use worker_host::release_batch as release_batch_workers;
pub use worker_host::run_embeddings as run_native_embeddings;
pub use worker_host::{release_idle as release_native_workers, run_pdf as run_native_pdf};

pub const MAX_INPUT: usize = 8 * 1024 * 1024;

fn dispatch(bytes: &[u8]) -> Result<Value, &'static str> {
    let request: Value = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
    if request["schema"] != "les.core.v1" {
        return Err("CORE_SCHEMA_MISMATCH");
    }
    match request["operation"].as_str() {
        Some("prepare_terms") => lexical::prepare(&request),
        Some("chat_profile") => Ok(chat_profiles::execute(&request["request"])),
        Some("tool_registry") => Ok(tool_registry::execute(&request["request"])),
        Some("filesystem") => Ok(filesystem::execute(&request["request"])),
        Some("unicode") => unicode::execute(&request["request"]),
        Some("lexical_tokens") => lexical::tokens(&request["request"]),
        Some("legacy_sparse") => legacy_sparse::execute(&request["request"]),
        Some("bm25") => bm25::execute(&request["request"]),
        Some("lexical_replace") => lexical_write::replace(&request["request"]),
        Some("lexical_initialize") => lexical_schema::execute(&request["request"]),
        Some("context") => context::execute(&request["request"]),
        Some("chat_context") => {
            chat_context::execute(&request["request"]).map(|value| json!({"value":value}))
        }
        Some("conversation_summary") => {
            conversation_summary::execute(&request["request"]).map(|value| json!({"value":value}))
        }
        Some("chat_commands") => {
            chat_commands::execute(&request["request"]).map(|value| json!({"value":value}))
        }
        Some("memory") => memory::execute(&request["request"]),
        Some("document") => document::execute(&request["request"]),
        Some("index_configuration") => index_configuration::execute(&request["request"]),
        Some("index_contract_publish") => index_bootstrap::execute(&request["request"]),
        Some("tables") => tables::execute(&request["request"]),
        Some("document_source") => Ok(document_source::execute(&request["request"])),
        Some("table_snapshot") => document::workbook::snapshot(&request["request"]),
        Some("workbook_index") => document::workbook_index::execute(&request["request"]),
        Some("citations") => citations::execute(&request["request"]),
        Some("harness") => harness::execute(&request["request"]),
        Some("harness_evidence") => harness_evidence::execute(&request["request"]),
        Some("index_journal") => index_journal::execute(&request["request"]),
        Some("index_sources") => index_sources::plan(&request["request"]),
        Some("retrieval_quality") => retrieval_quality::evaluate(&request["request"]),
        Some("fingerprint") => files::fingerprint(&request),
        Some("job_patch") => jobs::patch(&request),
        Some("job_recovery") => jobs::recovery(&request),
        Some("scan_sources") => source_scan::scan(&request),
        Some("worker_run") => worker_host::run(&request["request"]),
        Some("worker_pdf") => worker_host::run_pdf(&request["request"]),
        Some("worker_documents") => worker_host::run_documents(&request["request"]),
        Some("worker_tables") => worker_host::run_tables(&request["request"]),
        Some("worker_embeddings") => worker_host::run_embeddings(&request["request"]),
        Some("worker_mcp") => worker_host::run_mcp(&request["request"]),
        Some("worker_release_idle") => worker_host::release_idle(),
        Some("worker_stop") => worker_host::shutdown(),
        Some("skills") => Ok(skills::execute(&request["request"])),
        Some("tool_execution") => Ok(tool_execution::execute(&request["request"])),
        Some("workspace") => Ok(workspace::execute(&request["request"])),
        Some("catalog") => Ok(catalog::execute(&request["request"])),
        Some("trust") => trust::execute(&request["request"]),
        Some("model_profile") => model_profile::execute(&request["request"]),
        Some("model_request") => model_request::execute(&request["request"]),
        Some("model_capabilities") => model_capabilities::execute(&request["request"]),
        Some("model_secret") => model_secrets::execute(&request["request"]),
        Some("agent_files") => Ok(agent_files::execute(&request["request"])),
        Some("health") => Ok(
            json!({"abi": 1, "operations":["prepare_terms","unicode","lexical_tokens","legacy_sparse","bm25","lexical_replace","lexical_initialize","context","chat_context","conversation_summary","chat_commands","memory","document","document_source","document_chunks","document_docx","document_tokenizer","document_nodes","document_json","document_json_stream","document_source_nodes","document_conversion","document_dispatch","embedding_chunks","table_snapshot","tables","workbook_calculation","workbook_index","citations","harness","harness_evidence","chat_stream","chat_profile","tool_registry","tool_execution","filesystem","skills","skill_runtime","index_journal","index_points","index_run","index_configuration","index_contract_publish","chat_research","index_sources","retrieval_quality","fingerprint","job_patch","job_recovery","scan_sources","worker_run","worker_pdf","worker_documents","worker_tables","worker_embeddings","worker_mcp","worker_release_idle","worker_stop","workspace","catalog","trust","model_profile","model_request","model_capabilities","model_secret","agent_files"], "tokenizer":"les.lexical.v2"}),
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
    unsafe { reply(input, input_len, output_len, dispatch) }
}

type MeasureTokens = unsafe extern "C" fn(*const u8, usize) -> usize;
type IndexFile = extern "C" fn(usize, u64) -> u32;
type IndexBridge = unsafe extern "C" fn(u8, *const u8, usize, *mut usize) -> *const u8;

// A borrowed callback is decoded before the next invocation can replace it.
// All coordinators share these bounds and retain no foreign pointer.
unsafe fn bridge_json(
    adapter: IndexBridge,
    operation: u8,
    data: &Value,
    encode_error: &'static str,
    reply_error: &'static str,
) -> Result<Value, &'static str> {
    let input = serde_json::to_vec(data).map_err(|_| encode_error)?;
    if input.len() > 32 * 1024 * 1024 {
        return Err(encode_error);
    }
    let mut length = 0;
    let buffer = unsafe { adapter(operation, input.as_ptr(), input.len(), &mut length) };
    if buffer.is_null() || length == 0 || length > 32 * 1024 * 1024 {
        return Err(reply_error);
    }
    serde_json::from_slice(unsafe { slice::from_raw_parts(buffer, length) })
        .map_err(|_| reply_error)
}

#[no_mangle]
pub extern "C" fn les_core_chat_stream_new() -> *mut chat_stream::Stream {
    Box::into_raw(Box::new(chat_stream::Stream::default()))
}

/// # Safety
/// Pass one live stream handle and valid ABI buffers. Calls on the same
/// stream must be sequential, including disposal.
#[no_mangle]
pub unsafe extern "C" fn les_core_chat_stream_push(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    stream: *mut chat_stream::Stream,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let stream = stream.as_mut().ok_or("UPSTREAM_STREAM_EVENT_INVALID")?;
            let event =
                serde_json::from_slice(bytes).map_err(|_| "UPSTREAM_STREAM_EVENT_INVALID")?;
            stream.push(event, bytes.len())
        })
    }
}

/// # Safety
/// Dispose exactly once, after the last operation on the live handle.
#[no_mangle]
pub unsafe extern "C" fn les_core_chat_stream_free(stream: *mut chat_stream::Stream) {
    unsafe { drop(Box::from_raw(stream)) }
}

/// # Safety
/// The buffers and borrowed synchronous callback obey the index bridge ABI.
#[no_mangle]
pub unsafe extern "C" fn les_core_workbook_run(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexBridge>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("WORKBOOK_SNAPSHOT_INVALID")?;
            workbook_calculation::run(&request, |operation, data| {
                bridge_json(
                    adapter,
                    operation,
                    data,
                    "WORKBOOK_SNAPSHOT_INVALID",
                    "WORKBOOK_SNAPSHOT_INVALID",
                )
            })
        })
    }
}

/// # Safety
/// The buffers obey the `les_core_call` contract. The adapter borrows the input
/// only while called, and its returned response must remain valid until the next
/// callback or until this function returns. Neither pointer is retained.
#[no_mangle]
pub unsafe extern "C" fn les_core_index_points(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexBridge>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("INDEX_POINTS_ADAPTER_FAILED")?;
            index_points::run(&request, |operation, data| {
                let input = serde_json::to_vec(data).map_err(|_| "INDEX_POINTS_INVALID")?;
                let mut length = 0;
                let buffer = adapter(operation, input.as_ptr(), input.len(), &mut length);
                if buffer.is_null() || length == 0 || length > 32 * 1024 * 1024 {
                    return Err("INDEX_POINTS_ADAPTER_FAILED");
                }
                serde_json::from_slice(slice::from_raw_parts(buffer, length))
                    .map_err(|_| "INDEX_POINTS_ADAPTER_FAILED")
            })
        })
    }
}

/// # Safety
/// The buffers obey the `les_core_call` contract. The file adapter must remain
/// valid for the entire call. The native dispatcher never stores it.
#[no_mangle]
pub unsafe extern "C" fn les_core_index_batch(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexFile>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("INDEX_BATCH_INVALID")?;
            index_dispatch::run(&request, |index, attempt| adapter(index, attempt))
        })
    }
}

/// # Safety
/// All buffers and the synchronous bridge obey the index-points ABI contract.
#[no_mangle]
pub unsafe extern "C" fn les_core_index_run(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexBridge>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("INDEX_BATCH_INVALID")?;
            index_batch::run(&request, |operation, data| {
                bridge_json(
                    adapter,
                    operation,
                    data,
                    "INDEX_BATCH_INVALID",
                    "INDEX_BATCH_ADAPTER_FAILED",
                )
            })
        })
    }
}

/// # Safety
/// The request/reply buffers and synchronous callback obey the index bridge ABI.
/// The coordinator never retains the callback after returning.
#[no_mangle]
pub unsafe extern "C" fn les_core_research_run(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexBridge>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("AGENT_REQUEST_INVALID")?;
            agent_loop::run_research(&request, |operation, data| {
                bridge_json(
                    adapter,
                    operation,
                    data,
                    "AGENT_REQUEST_INVALID",
                    "CHAT_IO_FAILED",
                )
            })
        })
    }
}

/// # Safety
/// Buffers and the borrowed synchronous callback obey the index bridge ABI.
#[no_mangle]
pub unsafe extern "C" fn les_core_source_nodes_run(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexBridge>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("DOCUMENT_SOURCE_NODES_INVALID")?;
            document::source_nodes::run(&request, |operation, data| {
                bridge_json(
                    adapter,
                    operation,
                    data,
                    "DOCUMENT_SOURCE_NODES_INVALID",
                    "DOCUMENT_SOURCE_NODES_INVALID",
                )
            })
        })
    }
}

/// # Safety
/// Buffers and the borrowed synchronous callback obey the index bridge ABI.
#[no_mangle]
pub unsafe extern "C" fn les_core_json_run(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    adapter: Option<IndexBridge>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            let adapter = adapter.ok_or("JSON_READ_FAILED")?;
            document::json_source::run(&request, |operation, data| {
                bridge_json(
                    adapter,
                    operation,
                    data,
                    "JSON_READ_FAILED",
                    "JSON_READ_FAILED",
                )
            })
        })
    }
}

/// # Safety
/// The buffers obey the `les_core_call` contract. The callback, when supplied, is
/// valid until return and reads the borrowed UTF-8 bytes only during the call.
/// Return `usize::MAX` when the tokenizer fails. No callback escapes this call.
#[no_mangle]
pub unsafe extern "C" fn les_core_split_chunks(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    tokenizer: Option<MeasureTokens>,
) -> *mut u8 {
    unsafe {
        reply(input, input_len, output_len, |bytes| {
            let request = serde_json::from_slice(bytes).map_err(|_| "CORE_INVALID_JSON")?;
            if let Some(tokenizer) = tokenizer {
                let measure = |text: &str| {
                    let cost = tokenizer(text.as_ptr(), text.len());
                    if cost == usize::MAX || cost > u32::MAX as usize {
                        Err("DOCUMENT_TOKENIZER_FAILED")
                    } else {
                        Ok(cost)
                    }
                };
                document::measure_chunks(&request, Some(&measure))
            } else {
                document::measure_chunks(&request, None)
            }
        })
    }
}

unsafe fn reply(
    input: *const u8,
    input_len: usize,
    output_len: *mut usize,
    operation: impl FnOnce(&[u8]) -> Result<Value, &'static str>,
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
        match operation(bytes) {
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
    #[test]
    fn json_transport_keeps_finite_metadata_numbers_bit_exact() {
        for index in 0..1000 {
            let original = 1793535000.0 + f64::from(index) / 10_000_000.0;
            let encoded = serde_json::to_vec(&json!({"timestamp":original})).unwrap();
            let decoded: Value = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(
                decoded["timestamp"].as_f64().unwrap().to_bits(),
                original.to_bits()
            );
        }
    }
}
