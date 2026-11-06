//! Access to the Qdrant instance owned by this launcher.
mod sections;
use crate::{error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::time::Duration;

pub const PATH: &str = "/api/light/internal-index-store";

pub struct Store {
    target: reqwest::Url,
    key: String,
    token: String,
}

impl Store {
    pub fn from_env(token: String) -> Result<Option<Self>, &'static str> {
        let Ok(raw) = std::env::var("LES_LIGHT_QDRANT_URL") else {
            return Ok(None);
        };
        let target = reqwest::Url::parse(&raw).map_err(|_| "Invalid owned Qdrant URL")?;
        if target.scheme() != "http"
            || target.host_str() != Some("127.0.0.1")
            || target.port().is_none()
            || target.path() != "/"
            || target.query().is_some()
            || target.fragment().is_some()
            || !target.username().is_empty()
            || target.password().is_some()
        {
            return Err("Invalid owned Qdrant URL");
        }
        let key =
            std::env::var("LES_LIGHT_QDRANT_API_KEY").map_err(|_| "Missing owned Qdrant key")?;
        if key.is_empty() {
            return Err("Missing owned Qdrant key");
        }
        Ok(Some(Self { target, key, token }))
    }
}

fn valid_id(value: &Value) -> bool {
    value.as_str().is_some_and(|id| {
        id.len() == 36
            && id.bytes().enumerate().all(|(position, byte)| {
                if matches!(position, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    })
}

fn collection_name(body: &Value) -> Option<&str> {
    body["collection"].as_str().filter(|value| {
        !matches!(*value, "" | "." | "..")
            && value.len() <= 255
            && !value
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    })
}

fn mutation(body: &Value) -> Option<(&str, reqwest::Method, bool, Value)> {
    let collection = collection_name(body)?;
    let action = body["action"].as_str()?;
    match action {
        "upsert" => {
            let points = body["points"].as_array()?;
            if points.is_empty()
                || points.len() > 512
                || !points.iter().all(|point| {
                    valid_id(&point["id"])
                        && point["payload"].is_object()
                        && (point["vector"].is_array() || point["vector"].is_object())
                })
            {
                return None;
            }
            Some((
                collection,
                reqwest::Method::PUT,
                false,
                json!({"points":points}),
            ))
        }
        "discard" | "retire" => {
            let ids = body["ids"].as_array()?;
            if ids.is_empty() || ids.len() > 50_000 || !ids.iter().all(valid_id) {
                return None;
            }
            let selector = if action == "retire" {
                let dataset = body["dataset_id"]
                    .as_str()
                    .filter(|value| !value.is_empty())?;
                let name = body["file_name"]
                    .as_str()
                    .filter(|value| !value.is_empty())?;
                json!({"filter":{"must":[{"key":"dataset_id","match":{"value":dataset}},
                    {"key":"file_name","match":{"value":name}}],"must_not":[{"has_id":ids}]}})
            } else {
                json!({"points":ids})
            };
            Some((collection, reqwest::Method::POST, true, selector))
        }
        _ => None,
    }
}

pub async fn handle(context: &Context, request: Request) -> Response {
    let Some(store) = &context.index_store else {
        return error(StatusCode::NOT_FOUND, "Not Found");
    };
    if request.method() != "POST"
        || request.headers().contains_key("origin")
        || !request
            .headers()
            .get("x-les-index-token")
            .is_some_and(|token| {
                token.as_bytes().len() == store.token.len()
                    && store
                        .token
                        .bytes()
                        .zip(token.as_bytes())
                        .fold(0, |difference, (expected, actual)| {
                            difference | (expected ^ actual)
                        })
                        == 0
            })
    {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    let Ok(bytes) = to_bytes(request.into_body(), 64 * 1024 * 1024).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "INDEX_BATCH_TOO_LARGE");
    };
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID");
    };
    if body["action"] == "read_sections" {
        return sections::handle(context, store, &body).await;
    }
    let Some((collection, method, delete, payload)) = mutation(&body) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID");
    };
    let mut target = store.target.clone();
    {
        let mut segments = target
            .path_segments_mut()
            .expect("HTTP URL has path segments");
        segments
            .pop_if_empty()
            .extend(["collections", collection, "points"]);
        if delete {
            segments.push("delete");
        }
    }
    target.query_pairs_mut().append_pair("wait", "true");
    let result = context
        .client
        .request(method, target)
        .header("api-key", &store.key)
        .header("content-type", "application/json")
        .body(payload.to_string())
        .timeout(Duration::from_secs(60))
        .send()
        .await;
    if let Ok(response) = result {
        if response.status().is_success() {
            if let Ok(bytes) = response.bytes().await {
                if serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| {
                    value["status"] == "ok" && value["result"]["status"] == "completed"
                }) {
                    if delete {
                        eprintln!(
                            "{}",
                            json!({"event":"index_store_committed","action":body["action"],
                            "point_ids":body["ids"].as_array().map(Vec::len)})
                        );
                    }
                    return json_response(
                        StatusCode::OK,
                        json!({"status":"completed"}),
                        "rust.index_store",
                    );
                }
            }
        } else {
            eprintln!(
                "{}",
                json!({"event":"index_store_rejected","action":body["action"],
                "upstream_status":response.status().as_u16()})
            );
        }
    }
    error(StatusCode::BAD_GATEWAY, "INDEX_STORAGE_WRITE_FAILED")
}
