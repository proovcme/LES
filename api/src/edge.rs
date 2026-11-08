//! Temporary wire compatibility for retained Qdrant clients; native paths call Shard directly.
use crate::{error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{sync::Arc, time::Instant};
pub fn handles(path: &str) -> bool {
    path == "/aliases" || path == "/collections" || path.starts_with("/collections/")
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let started = Instant::now();
    let Some(store) = context.index_store.as_ref() else {
        return error(StatusCode::NOT_FOUND, "Not Found");
    };
    if !store.authorized(request.headers()) {
        return error(StatusCode::UNAUTHORIZED, "Storage authentication required");
    }
    if let Some(code) = store.recovery_error() {
        return error(StatusCode::SERVICE_UNAVAILABLE, code);
    }
    let Some(shard) = store.edge.clone() else {
        return error(StatusCode::NOT_FOUND, "Not Found");
    };
    let Ok(permit) = context.workspace_slots.clone().try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "EDGE_BUSY");
    };
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let fields = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map(|v| v.into_owned())
        })
        .collect::<Result<Vec<_>, _>>();
    let Ok(fields) = fields else {
        return error(StatusCode::BAD_REQUEST, "EDGE_REQUEST_INVALID");
    };
    if fields == ["aliases"] && method == "GET" {
        return json_response(
            StatusCode::OK,
            json!({"result":{"aliases":[]},"status":"ok","time":started.elapsed().as_secs_f64()}),
            "rust.edge",
        );
    }
    if fields.len() == 1 && method == "GET" {
        return json_response(
            StatusCode::OK,
            json!({"result":{"collections":[{"name":shard.collection()}]},"status":"ok","time":started.elapsed().as_secs_f64()}),
            "rust.edge",
        );
    }
    if fields.get(1).map(String::as_str) != Some(shard.collection()) {
        return error(StatusCode::NOT_FOUND, "EDGE_COLLECTION_NOT_FOUND");
    }
    if fields.get(2).map(String::as_str) == Some("aliases") && fields.len() == 3 && method == "GET"
    {
        return json_response(
            StatusCode::OK,
            json!({"result":{"aliases":[]},"status":"ok","time":started.elapsed().as_secs_f64()}),
            "rust.edge",
        );
    }
    let action = match (
        method.as_str(),
        fields[2..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
    ) {
        ("GET", []) => "collection_info",
        ("GET", ["exists"]) => "exists",
        ("POST", ["points", "scroll"]) => "scroll",
        ("POST", ["points", "count"]) => "count",
        ("POST", ["points", "query"]) => "query",
        ("POST", ["points"]) => "retrieve",
        ("PUT", ["points"]) => "upsert",
        ("POST", ["points", "delete"]) => "delete",
        ("PUT", ["index"]) => "create_payload_index",
        ("POST", ["points", "payload"]) => "set_payload",
        ("PUT", ["points", "payload"]) => "overwrite_payload",
        ("POST", ["points", "payload", "delete"]) => "delete_payload",
        ("POST", ["points", "payload", "clear"]) => "clear_payload",
        _ => return error(StatusCode::NOT_IMPLEMENTED, "EDGE_OPERATION_NOT_SUPPORTED"),
    };
    let bytes = match to_bytes(request.into_body(), 64 * 1024 * 1024).await {
        Ok(v) => v,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "EDGE_REQUEST_TOO_LARGE"),
    };
    let body = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(_) => return error(StatusCode::BAD_REQUEST, "EDGE_REQUEST_INVALID"),
        }
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let guarded = matches!(action, "query" | "retrieve" | "scroll");
        let generation = if guarded {
            Some(shard.generation()?)
        } else {
            None
        };
        let value = shard.call(action, &body)?;
        if guarded && Some(shard.generation()?) != generation {
            return Err("INDEX_CHANGED_DURING_SEARCH");
        }
        Ok(value)
    })
    .await;
    match result {
        Ok(Ok(value)) => json_response(
            StatusCode::OK,
            json!({"result":value,"status":"ok","time":started.elapsed().as_secs_f64()}),
            "rust.edge",
        ),
        Ok(Err(code)) => error(
            if matches!(
                code,
                "INDEX_RECOVERY_REQUIRED" | "INDEX_CHANGED_DURING_SEARCH"
            ) {
                StatusCode::CONFLICT
            } else if code == "EDGE_OPERATION_NOT_SUPPORTED" || code == "EDGE_QUERY_NOT_SUPPORTED" {
                StatusCode::NOT_IMPLEMENTED
            } else {
                StatusCode::BAD_REQUEST
            },
            code,
        ),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "EDGE_WORKER_FAILED"),
    }
}
