//! Private canonical inference: SQLite revision, policy, credentials, payload and stream are native.
use crate::{error, Context};
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{Method, StatusCode},
    response::Response,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
pub const PATH: &str = "/api/light/internal-inference";
pub const PROTOCOL: &str = "les.inference.v1";

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != Method::POST
        || context
            .local_model
            .as_ref()
            .is_none_or(|b| !b.authorized(request.headers()))
    {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    let bytes = match to_bytes(request.into_body(), 4 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "UPSTREAM_REQUEST_TOO_LARGE"),
    };
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "INFERENCE_REQUEST_INVALID"),
    };
    if value["schema"] != "les.inference.v1" || !value["request"].is_object() {
        return error(StatusCode::BAD_REQUEST, "INFERENCE_REQUEST_INVALID");
    }
    let selector = match (value["role"].as_str(), value["revision_id"].as_str()) {
        (Some(role), None) if matches!(role, "answer" | "local_fallback") => json!({"role":role}),
        (None, Some(id)) if !id.is_empty() && id.len() <= 256 => json!({"revision_id":id}),
        _ => return error(StatusCode::BAD_REQUEST, "INFERENCE_SELECTOR_INVALID"),
    };
    let role = value["role"].as_str().unwrap_or("answer").to_string();
    let streaming = value["streaming"].as_bool().unwrap_or(true);
    let seconds = value
        .get("timeout_seconds")
        .map_or(Some(120), Value::as_u64)
        .filter(|v| (1..=600).contains(v));
    let Some(seconds) = seconds else {
        return error(StatusCode::BAD_REQUEST, "INFERENCE_TIMEOUT_INVALID");
    };
    let request = value["request"].as_object_mut().unwrap();
    for (key, default) in [
        ("tools", json!([])),
        ("reasoning_enabled", json!(false)),
        ("response_format", Value::Null),
        ("temperature", Value::Null),
    ] {
        request.entry(key).or_insert(default);
    }
    let body=Body::from_stream(stream::once(async move {
        let result:Result<Body,String>=async {
            let permit=context.workspace_slots.clone().acquire_owned().await.map_err(|_|"MODEL_RUNTIME_UNAVAILABLE".to_string())?;
            let database=context.database.clone();
            let path=std::env::var_os("LES_ENV_PATH").map(std::path::PathBuf::from).unwrap_or_else(||context.state_root.join(".env"));
            let resolved=tokio::task::spawn_blocking(move||{
                let _permit=permit;
                let call=|action,args|les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,"action":action,"arguments":args}));
                let initialized=call("model_registry_init",json!({}));
                if initialized["status"]!="completed"{return Err("MODEL_REGISTRY_UNAVAILABLE".to_string());}
                let execution=call("model_execution_snapshot",selector);
                if execution["status"]!="completed"{return Err("MODEL_REGISTRY_UNAVAILABLE".to_string());}
                crate::model_runtime::resolve(&execution["value"],&role,&path)
            }).await.map_err(|_|"MODEL_RUNTIME_UNAVAILABLE".to_string())??;
            if resolved.endpoint.locality=="remote" && value["allow_remote"]!=true {return Err("CLOUD_CONSENT_REQUIRED".into());}
            let native=resolved.snapshot["transport_options"]["chat_protocol"]=="native_chat_v1";
            let observed_stream=les_core::model_capabilities::require(&resolved.snapshot,&["streaming".into()],None).is_ok();
            if value["require_stream"]==true && !observed_stream {return Err("CAPABILITY_REQUIRED: streaming".into());}
            let streaming=streaming && (native || observed_stream);
            let upstream_port=context.upstream_port().unwrap_or(0);
            let body=resolved.chat(&value["request"],streaming,Duration::from_secs(seconds),[context.listen_port,upstream_port]).await?;
            if !streaming && value["streaming"]==true {
                let event=format!("{}\n",json!({"progress":{"label":"Модель готовит ответ целиком — поток не поддерживается"}}));
                return Ok(Body::from_stream(stream::once(async move{Ok::<_,axum::Error>(axum::body::Bytes::from(event))}).chain(body.into_data_stream())));
            }
            Ok(body)
        }.await;
        match result {
            Ok(body)=>body.into_data_stream(),
            Err(code)=>{
                eprintln!("{}",json!({"event":"inference_failed","code":code.split(':').next().unwrap_or(&code)}));
                Body::from(format!("{}\n",json!({"error":code}))).into_data_stream()
            }
        }
    }).flatten());
    Response::builder()
        .status(StatusCode::OK)
        .header("x-les-handler", "rust.inference")
        .header("x-les-model-protocol", PROTOCOL)
        .header("cache-control", "no-store")
        .header("content-type", "application/x-ndjson")
        .body(body)
        .expect("valid native inference response")
}
