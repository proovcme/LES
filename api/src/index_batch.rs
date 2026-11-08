//! A complete native batch executor. No callback is implemented by application Python.
use crate::{error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};
pub(crate) mod configuration;
pub const PATH: &str = "/api/light/internal-index-batch";

fn failure(code: &str) -> Value {
    json!({"error":{"code":code.split(':').next().unwrap_or(code),"message":code,"error_type":"NativeIndexError"}})
}

fn model_key(value: &str) -> String {
    let value = value.trim().to_lowercase();
    let value = value.strip_suffix(":latest").unwrap_or(&value);
    match value {
        "baai/bge-m3" => "bge-m3".into(),
        "qwen/qwen3-embedding-0.6b" => "qwen3-embedding-0.6b".into(),
        "qwen/qwen3-embedding-4b" => "qwen3-embedding-4b".into(),
        "qwen/qwen3-embedding-8b" => "qwen3-embedding-8b".into(),
        _ => value.into(),
    }
}

fn same_path(supplied: &Path, owned: &Path) -> bool {
    if !supplied.is_absolute() {
        return false;
    }
    if supplied == owned {
        return true;
    }
    match (supplied.canonicalize(), owned.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => {
            supplied.file_name() == owned.file_name()
                && supplied.parent().zip(owned.parent()).is_some_and(|(a, b)| {
                    a.canonicalize()
                        .ok()
                        .zip(b.canonicalize().ok())
                        .is_some_and(|(a, b)| a == b)
                })
        }
    }
}

fn vectors(
    bytes: &[u8],
    count: usize,
    dimension: usize,
    model: &str,
) -> Result<Value, &'static str> {
    let split = bytes
        .iter()
        .position(|v| *v == b'\n')
        .ok_or("EMBEDDING_RESPONSE_INVALID")?;
    let info: Value =
        serde_json::from_slice(&bytes[..split]).map_err(|_| "EMBEDDING_RESPONSE_INVALID")?;
    if info["count"].as_u64() != Some(count as u64)
        || info["dimension"].as_u64() != Some(dimension as u64)
        || info["model_id"]
            .as_str()
            .is_some_and(|v| model_key(v) != model_key(model))
    {
        return Err("EMBEDDING_INDEX_MODEL_MISMATCH");
    }
    let raw = &bytes[split + 1..];
    if count.checked_mul(dimension).and_then(|v| v.checked_mul(8)) != Some(raw.len()) {
        return Err("EMBEDDING_RESPONSE_INVALID");
    }
    let mut result = Vec::with_capacity(count);
    for row in raw.chunks_exact(dimension * 8) {
        let mut values = Vec::with_capacity(dimension);
        for item in row.chunks_exact(8) {
            let value =
                f64::from_le_bytes(item.try_into().map_err(|_| "EMBEDDING_RESPONSE_INVALID")?);
            if !value.is_finite() {
                return Err("EMBEDDING_RESPONSE_INVALID");
            }
            values.push(value);
        }
        result.push(values);
    }
    Ok(json!(result))
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != "POST"
        || context
            .index_store
            .as_ref()
            .is_none_or(|s| !s.authorized_internal(request.headers()))
    {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    if let Some(code) = context
        .index_store
        .as_ref()
        .and_then(|s| s.recovery_error())
    {
        return error(StatusCode::SERVICE_UNAVAILABLE, code);
    }
    let bytes = match to_bytes(request.into_body(), 4 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "INDEX_BATCH_TOO_LARGE"),
    };
    let input: Value = match serde_json::from_slice(&bytes) {
        Ok(Value::Object(v)) => Value::Object(v),
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID"),
    };
    execute(context, input).await
}

pub(crate) async fn execute(context: Arc<Context>, mut input: Value) -> Response {
    let Some(store) = context.index_store.as_ref() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "INDEX_STORAGE_UNAVAILABLE");
    };
    if let Some(code) = store.recovery_error() {
        return error(StatusCode::SERVICE_UNAVAILABLE, code);
    }
    // All filesystem/worker authority comes from the owned native service.
    // A retained caller may supply a frozen algorithm/model descriptor, never a process to run.
    let root = store.content_root(&context);
    let source_root = context.state_root.join("storage/datasets");
    let spool = context.state_root.join("data/index-publish");
    for (key, expected) in [
        ("database", context.database.as_path()),
        ("content_root", root.as_path()),
        ("spool_directory", spool.as_path()),
    ] {
        if input[key]
            .as_str()
            .map(Path::new)
            .is_none_or(|given| !same_path(given, expected))
        {
            return error(StatusCode::CONFLICT, "INDEX_BATCH_SCOPE_MISMATCH");
        }
        input[key] = json!(expected);
    }
    if input.get("source_root").is_some_and(|v| {
        v.as_str()
            .map(Path::new)
            .is_none_or(|p| !same_path(p, &source_root))
    }) {
        return error(StatusCode::CONFLICT, "INDEX_BATCH_SCOPE_MISMATCH");
    }
    input["source_root"] = json!(source_root);
    if input["schema"] != "les.index-batch.v1" || !input["format_worker"].is_object() {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID");
    }
    if input["dataset_id"].as_str().is_none_or(|v| {
        v.is_empty()
            || v.len() > 160
            || matches!(v, "." | "..")
            || v.contains(['/', '\\', ':', '\0'])
    }) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID");
    }
    let program = match std::env::current_exe() {
        Ok(v) => v,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "FORMAT_WORKER_UNAVAILABLE"),
    };
    input["format_worker"]["program"] = json!(program);
    input["format_worker"]["directory"] = json!(program.parent());
    match les_core::system_memory::available() {
        Ok(bytes) if bytes >= 4 * 1024 * 1024 * 1024 => {}
        Ok(_) => return error(StatusCode::TOO_MANY_REQUESTS, "INDEX_MEMORY_GUARD"),
        Err(code) => return error(StatusCode::SERVICE_UNAVAILABLE, code),
    }
    let permit = match context.workspace_slots.clone().try_acquire_owned() {
        Ok(v) => v,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "INDEX_BATCH_BUSY"),
    };
    let runtime = tokio::runtime::Handle::current();
    let owner = context.clone();
    let value=tokio::task::spawn_blocking(move|| {
        let _permit=permit;
        crate::document_dispatch::freeze_ocr(&mut input["format_worker"],&context.state_root);
        let dimension=input["embedding"]["vector_size"].as_u64().or_else(||input["embedding"]["vector_size"].as_str()?.parse().ok()).filter(|v|(1..=65536).contains(v)).ok_or("EMBEDDING_DESCRIPTOR_INVALID")? as usize;
        let model=input["embedding"]["model_id"].as_str().ok_or("EMBEDDING_DESCRIPTOR_INVALID")?;
        let planned=les_core::plan_index_sources(&json!({"database":context.database,
            "root":source_root.join(input["dataset_id"].as_str().ok_or("INDEX_BATCH_INVALID")?),
            "dataset_id":input["dataset_id"],"limit":if input["sources"].is_array(){Value::Null}else{input["limit"].clone()}}))?;
        for file in planned["files"].as_array().ok_or("INDEX_SOURCE_PLAN_INVALID")? {
            if let Some(selected)=input["sources"].as_array() {
                if !selected.iter().any(|s|s["file_name"]==file["db_key"] && s["source_path"]==file["source_path"]){continue;}
            }
            if let Some(path)=file["path"].as_str() {
                if !les_core::supports_native_document_dispatch(Path::new(path)){return Err("DOCUMENT_NATIVE_DISPATCH_REQUIRED");}
            }
        }
        if let Some(revision)=input["embedding_execution"]["revision_id"].as_str() {
            let result=les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":context.database,
                "action":"model_registry_revision","arguments":{"revision_id":revision}}));
            if result["status"]!="completed" {return Err("MODEL_REGISTRY_UNAVAILABLE");}
            if result["value"]["model_id"].as_str().is_none_or(|saved|model_key(saved)!=model_key(model)) {return Err("EMBEDDING_INDEX_MODEL_MISMATCH");}
        } else if std::env::var_os("LES_NATIVE_EMBEDDING_SPEC").is_none() {
            return Err("EMBEDDING_NATIVE_EXECUTION_REQUIRED");
        }
        let used_embedding=std::cell::Cell::new(false);
        let encode=|texts:&Value|->Result<Value,String>{
            used_embedding.set(true);
            let count=texts.as_array().filter(|v|!v.is_empty()).ok_or("EMBEDDING_INPUT_INVALID")?.len();
            if let Some(revision)=input["embedding_execution"]["revision_id"].as_str() {
                let bytes=runtime.block_on(crate::embedding_inference::execute(context.clone(),&json!({"schema":"les.embedding-inference.v1","revision_id":revision,"inputs":texts,"dimension":dimension,"allow_remote":input["embedding_execution"]["allow_remote"],"response_limit":8*1024*1024,"timeout_seconds":120})))?;
                vectors(&bytes,count,dimension,model).map_err(str::to_owned)
            } else {
                if input["embedding"]["backend"]!="sentence_transformers" {return Err("EMBEDDING_NATIVE_EXECUTION_REQUIRED".into());}
                let value=crate::embedding_worker::encode(&json!({"model_id":model,"dimension":dimension,"texts":texts}))?;
                Ok(value["vectors"].clone())
            }
        };
        let mut result=les_core::index_batch::run(&input,|operation,data| {
            let result=match operation {
                // Conversion must be selected and executed by the native dispatcher.
                0=>return Ok(failure("DOCUMENT_NATIVE_DISPATCH_REQUIRED")),
                1=>encode(data),
                2=>runtime.block_on(async {
                    let response=crate::index_store::execute(context.clone(),data.clone()).await;
                    let status=response.status();
                    let bytes=to_bytes(response.into_body(),4*1024*1024).await.map_err(|_|"INDEX_STORAGE_RESPONSE_INVALID")?;
                    let value:Value=serde_json::from_slice(&bytes).map_err(|_|"INDEX_STORAGE_RESPONSE_INVALID")?;
                    if !status.is_success(){return Err(value["detail"].as_str().unwrap_or("INDEX_STORAGE_WRITE_FAILED").to_owned());}
                    Ok(value)
                }),
                _=>return Ok(failure("INDEX_BATCH_ADAPTER_FAILED")),
            };
            Ok(result.unwrap_or_else(|code|failure(&code)))
        });
        if input["release_workers_after"]==true {
            let mut released=0;
            if used_embedding.get() {
                if let Some(revision)=input["embedding_execution"]["revision_id"].as_str() {
                    match runtime.block_on(crate::embedding_inference::release_owned(context.clone(),revision)) {
                        Ok(true)=>released=1,
                        Ok(false)=>{},
                        Err(code)=>eprintln!("{}",json!({"event":"index_embedding_release_failed","code":code.split(':').next().unwrap_or(&code)})),
                    }
                }
            }
            let cleanup=les_core::release_batch_workers().unwrap_or_else(|code|json!({"code":code}));
            if let Ok(value)=&mut result {value["embedding_cleanup"]=json!({"released":released,"workers":cleanup});}
        }
        result
    }).await;
    match value {
        Ok(Ok(value)) => {
            crate::dataset_reader::notify(&owner);
            json_response(StatusCode::OK, value, "rust.index-batch")
        }
        Ok(Err(code)) => {
            eprintln!(
                "{}",
                json!({"event":"native_batch_admission_stopped","code":code})
            );
            error(
                if code == "DOCUMENT_NATIVE_DISPATCH_REQUIRED" {
                    StatusCode::PRECONDITION_FAILED
                } else {
                    StatusCode::CONFLICT
                },
                code,
            )
        }
        Err(_) => error(StatusCode::BAD_GATEWAY, "INDEX_BATCH_OWNER_FAILED"),
    }
}
