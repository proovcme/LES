//! Canonical embedding execution from the saved, explicitly selected model revision.
use crate::{error, model_runtime, ollama_embed, Context};
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::StatusCode,
    response::Response,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
pub const PATH: &str = "/api/light/internal-inference/embeddings";

pub(crate) async fn release_owned(context: Arc<Context>, revision: &str) -> Result<bool, String> {
    // Only release the exact loopback Ollama embedding model used by this batch.
    // External/shared services and unrelated models are never unloaded.
    if std::env::var_os("LES_NATIVE_EMBEDDING_SPEC").is_some() {
        return Ok(false);
    }
    let database = context.database.clone();
    let revision = revision.to_owned();
    let env = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    let resolved=tokio::task::spawn_blocking(move|| {
        let value=les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,"action":"model_execution_snapshot","arguments":{"revision_id":revision}}));
        if value["status"]!="completed"{return Err("MODEL_REGISTRY_UNAVAILABLE".into());}
        model_runtime::resolve(&value["value"],"embeddings",&env)
    }).await.map_err(|_|"MODEL_RUNTIME_UNAVAILABLE")??;
    if resolved.endpoint.locality != "loopback"
        || resolved.snapshot["transport_options"]["embedding_protocol"] != "native_embedding_v1"
    {
        return Ok(false);
    }
    let mut url = resolved.endpoint.url;
    let base = url.path().trim_end_matches('/');
    let path = format!(
        "{}/api/embed",
        base.strip_suffix("/api/v1")
            .or_else(|| base.strip_suffix("/v1"))
            .unwrap_or(base)
    );
    url.set_path(&path);
    if url.port_or_known_default() == Some(context.listen_port)
        || url.port_or_known_default() == context.upstream_port()
    {
        return Err("MODEL_BRIDGE_TARGET_REJECTED".into());
    }
    let reply = crate::model_http::open(
        url,
        resolved.endpoint.addresses,
        resolved.authorization,
        json!({"model":resolved.revision["model_id"],"input":[],"keep_alive":0})
            .to_string()
            .into_bytes(),
        Duration::from_secs(8),
        false,
    )
    .await?;
    if !reply.status.is_success() {
        return Err("EMBEDDING_RELEASE_FAILED".into());
    }
    let mut source = reply.source;
    let mut received = 0;
    while let Some(part) = source.next().await {
        received += part?.len();
        if received > 65536 {
            return Err("EMBEDDING_RELEASE_FAILED".into());
        }
    }
    Ok(true)
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != "POST"
        || context
            .local_model
            .as_ref()
            .is_none_or(|b| !b.authorized(request.headers()))
    {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    let bytes = match to_bytes(request.into_body(), 3 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "EMBEDDING_INPUT_INVALID"),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "EMBEDDING_INPUT_INVALID"),
    };
    let response = execute(context, &value).await;
    match response {
        Ok(bytes) => Response::builder()
            .status(200)
            .header("content-type", "application/octet-stream")
            .header("x-les-handler", "rust.embedding-inference")
            .header("x-les-model-protocol", ollama_embed::PROTOCOL)
            .header("cache-control", "no-store")
            .body(Body::from(bytes))
            .expect("fixed embedding headers"),
        Err(code) => {
            eprintln!(
                "{}",
                json!({"event":"embedding_inference_stopped","code":code.split(':').next().unwrap_or(&code)})
            );
            error(StatusCode::BAD_GATEWAY, &code)
        }
    }
}
pub(crate) async fn execute(context: Arc<Context>, value: &Value) -> Result<Vec<u8>, String> {
    let permit = context
        .workspace_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "EMBEDDING_BUSY")?;
    if value["schema"] != "les.embedding-inference.v1" {
        return Err("EMBEDDING_REQUEST_INVALID".into());
    }
    let inputs = value["inputs"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or("EMBEDDING_INPUT_INVALID")?;
    let mut total = 0usize;
    for input in inputs {
        total = total.saturating_add(input.as_str().ok_or("EMBEDDING_INPUT_INVALID")?.len());
    }
    if total > 2 * 1024 * 1024 {
        return Err("EMBEDDING_INPUT_INVALID".into());
    }
    let revision = value["revision_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256)
        .ok_or("INFERENCE_SELECTOR_INVALID")?
        .to_owned();
    let database = context.database.clone();
    let env_path = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    let resolved = tokio::task::spawn_blocking(move || {
        let result =
            les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,
            "action":"model_execution_snapshot","arguments":{"revision_id":revision}}));
        if result["status"] != "completed" {
            return Err("MODEL_REGISTRY_UNAVAILABLE".into());
        }
        model_runtime::resolve(&result["value"], "embeddings", &env_path)
    })
    .await
    .map_err(|_| "MODEL_RUNTIME_UNAVAILABLE")??;
    if resolved.endpoint.locality == "remote" && value["allow_remote"] != true {
        return Err("CLOUD_CONSENT_REQUIRED".into());
    }
    let bytes_limit = value["response_limit"]
        .as_u64()
        .unwrap_or(1024 * 1024)
        .clamp(1024, 8 * 1024 * 1024) as usize;
    let count = inputs.len();
    let seconds = value["timeout_seconds"]
        .as_u64()
        .filter(|v| (1..=600).contains(v))
        .ok_or("INFERENCE_TIMEOUT_INVALID")?;
    let (raw, format) = if std::env::var_os("LES_NATIVE_EMBEDDING_SPEC").is_some() {
        // An explicit local manifest pins its connection revision. Failure never falls through to HTTP.
        if resolved.endpoint.locality != "loopback" {
            return Err("EMBEDDING_LOCAL_BINDING_REQUIRED".into());
        }
        let request = json!({"model_id":resolved.revision["model_id"],"connection_revision_id":resolved.revision["revision_id"],
            "texts":inputs,"dimension":value["dimension"],"timeout_ms":seconds*1000});
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::embedding_worker::encode(&request)
        })
        .await
        .map_err(|_| "EMBEDDING_WORKER_FAILED")?;
        if value["release_after"] == true {
            let _ = les_core::release_native_workers();
        }
        let result = result?;
        (
            json!({"model":resolved.revision["model_id"],"embeddings":result["vectors"]})
                .to_string()
                .into_bytes(),
            ollama_embed::Format::Ollama,
        )
    } else {
        let native =
            resolved.snapshot["transport_options"]["embedding_protocol"] == "native_embedding_v1";
        let mut url = resolved.endpoint.url.clone();
        let base = url.path().trim_end_matches('/');
        let path = if native {
            format!(
                "{}/api/embed",
                base.strip_suffix("/api/v1")
                    .or_else(|| base.strip_suffix("/v1"))
                    .unwrap_or(base)
            )
        } else {
            format!(
                "{}/embeddings",
                if base.ends_with("/v1") {
                    base.to_owned()
                } else {
                    format!("{base}/v1")
                }
            )
        };
        url.set_path(&path);
        let upstream_port = context.upstream_port().unwrap_or(0);
        if resolved.endpoint.locality == "loopback"
            && url
                .port_or_known_default()
                .is_some_and(|v| [context.listen_port, upstream_port].contains(&v))
        {
            return Err("MODEL_BRIDGE_TARGET_REJECTED".into());
        }
        let mut body = json!({"model":resolved.revision["model_id"],"input":inputs});
        if native {
            body["truncate"] = json!(false);
            body["keep_alive"] = if value["release_after"] == true {
                json!(0)
            } else {
                json!("60s")
            };
            body["options"] = json!({"num_ctx":resolved.profile["input_token_limit"]});
        }
        let reply = crate::model_http::open(
            url,
            resolved.endpoint.addresses.clone(),
            resolved.authorization.clone(),
            body.to_string().into_bytes(),
            Duration::from_secs(seconds),
            false,
        )
        .await?;
        if !reply.status.is_success() {
            return Err(format!("UPSTREAM_HTTP_ERROR: {}", reply.status.as_u16()));
        }
        let mut source = reply.source;
        let mut bytes = Vec::new();
        while let Some(part) = source.next().await {
            let part = part?;
            if bytes.len() + part.len() > bytes_limit {
                return Err("UPSTREAM_RESPONSE_TOO_LARGE".into());
            }
            bytes.extend_from_slice(&part);
        }
        (
            bytes,
            if native {
                ollama_embed::Format::Ollama
            } else {
                ollama_embed::Format::OpenAi
            },
        )
    };
    if raw.len() > bytes_limit {
        return Err("UPSTREAM_RESPONSE_TOO_LARGE".into());
    }
    ollama_embed::encode(&raw, count, format).map_err(str::to_owned)
}
