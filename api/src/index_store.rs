//! Access to the Qdrant instance owned by this launcher.
mod bootstrap;
mod cache;
mod publication;
mod search;
pub(crate) mod sections;
use crate::{error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

pub const PATH: &str = "/api/light/internal-index-store";

pub struct Store {
    target: reqwest::Url,
    key: String,
    token: String,
    #[cfg(feature = "edge")]
    pub(crate) edge: tokio::sync::OnceCell<Arc<les_vector::Shard>>,
    #[cfg(feature = "edge")]
    pub(crate) edge_root: Option<std::path::PathBuf>,
    #[cfg(feature = "edge")]
    recovery: tokio::sync::OnceCell<Result<(), &'static str>>,
}

impl Store {
    pub(crate) fn authorized_internal(&self, headers: &axum::http::HeaderMap) -> bool {
        !headers.contains_key("origin")
            && headers.get("x-les-index-token").is_some_and(|v| {
                v.as_bytes().len() == self.token.len()
                    && v.as_bytes()
                        .iter()
                        .zip(self.token.bytes())
                        .fold(0, |d, (a, b)| d | (a ^ b))
                        == 0
            })
    }
    pub(crate) async fn revision(
        &self,
        context: &Context,
        collection: &str,
    ) -> Result<Value, &'static str> {
        let path = self
            .content_root(context)
            .join(".index-replacements")
            .join(&les_core::sha256_hex(collection.as_bytes())[..24])
            .join("journal.sqlite");
        tokio::task::spawn_blocking(move || {
            let value =
                les_core::index_journal::execute(&json!({"database":path,"action":"read"}))?
                    ["value"]
                    .clone();
            if !value.is_null() && value["phase"] != "idle" {
                return Err("INDEX_CHANGED_DURING_SEARCH");
            }
            Ok(value["revision"].clone())
        })
        .await
        .map_err(|_| "INDEX_CHANGED_DURING_SEARCH")?
    }
    pub(crate) fn content_root(&self, context: &Context) -> std::path::PathBuf {
        #[cfg(feature = "edge")]
        if let Some(root) = &self.edge_root {
            return root.join("content");
        }
        context.state_root.join("storage/datasets")
    }
    pub(crate) async fn refresh_postings(&self) -> Result<(), &'static str> {
        #[cfg(feature = "edge")]
        if let Some(edge) = self.edge.get() {
            let edge = edge.clone();
            tokio::task::spawn_blocking(move || {
                edge.refresh_postings(false, |event| {
                    eprintln!(
                        "{}",
                        json!({"event":"index_lexical_progress","progress":event})
                    );
                })
            })
            .await
            .map_err(|_| "INDEX_SPARSE_OWNER_FAILED")??;
        }
        Ok(())
    }
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
        Ok(Some(Self {
            target,
            key,
            token,
            #[cfg(feature = "edge")]
            edge: tokio::sync::OnceCell::new(),
            #[cfg(feature = "edge")]
            edge_root: None,
            #[cfg(feature = "edge")]
            recovery: tokio::sync::OnceCell::new(),
        }))
    }
    #[cfg(feature = "edge")]
    pub fn embedded(
        root: &std::path::Path,
        port: u16,
        key: String,
        token: String,
    ) -> Result<Self, &'static str> {
        if key.is_empty() || key.chars().any(char::is_control) {
            return Err("Invalid Edge storage key");
        }
        if !root.is_absolute() {
            return Err("EDGE_PATH_INVALID");
        }
        let edge = tokio::sync::OnceCell::new();
        if root.join("les-vector.sqlite").exists() && !les_vector::initialize::pending(root) {
            edge.set(Arc::new(les_vector::Shard::open(root)?))
                .map_err(|_| "EDGE_STORAGE_BUSY")?;
        }
        Ok(Self {
            edge,
            edge_root: Some(root.into()),
            recovery: tokio::sync::OnceCell::new(),
            target: reqwest::Url::parse(&format!("http://127.0.0.1:{port}/"))
                .map_err(|_| "Invalid Edge address")?,
            key,
            token,
        })
    }
    pub(crate) fn recovery_error(&self) -> Option<&'static str> {
        #[cfg(feature = "edge")]
        if self.edge_root.is_some() {
            if self.edge.get().is_none() {
                return Some("INDEX_NOT_CONFIGURED");
            }
            return match self.recovery.get() {
                Some(Ok(())) => None,
                Some(Err(code)) => Some(*code),
                None => Some("INDEX_RECOVERING"),
            };
        }
        None
    }
    #[cfg(feature = "edge")]
    pub(crate) fn authorized(&self, headers: &axum::http::HeaderMap) -> bool {
        !headers.contains_key("origin")
            && headers.get("api-key").is_some_and(|v| {
                v.as_bytes().len() == self.key.len()
                    && v.as_bytes()
                        .iter()
                        .zip(self.key.bytes())
                        .fold(0, |d, (a, b)| d | (a ^ b))
                        == 0
            })
    }
}

/// Run before admitting index requests; unrelated API screens stay responsive.
pub(crate) async fn start_recovery(context: Arc<Context>) -> Result<(), &'static str> {
    #[cfg(feature = "edge")]
    {
        if !context
            .index_store
            .as_ref()
            .is_some_and(|store| store.edge.get().is_some())
        {
            return Ok(());
        }
        let runtime = tokio::runtime::Handle::current();
        let worker_context = context.clone();
        eprintln!("{}", json!({"event":"index_startup_recovery_started"}));
        let result = tokio::task::spawn_blocking(move || {
            let context = worker_context;
            let store = context.index_store.as_ref().expect("owned Edge store");
            let collection = store.edge.get().expect("owned Edge shard").collection();
            runtime
                .block_on(publication::recover_file(
                    &context,
                    store,
                    &json!({"collection":collection}),
                ))
                .and_then(|result| {
                    if result["status"] == "legacy" {
                        Err("INDEX_RECOVERY_INCOMPLETE")
                    } else {
                        Ok(())
                    }
                })
        })
        .await
        .unwrap_or(Err("INDEX_RECOVERY_OWNER_FAILED"));
        eprintln!(
            "{}",
            match result {
                Ok(()) => json!({"event":"index_startup_recovery_completed"}),
                Err(code) => json!({"event":"index_startup_recovery_stopped","reason":code}),
            }
        );
        let _ = context
            .index_store
            .as_ref()
            .expect("owned Edge store")
            .recovery
            .set(result);
        return result;
    }
    #[cfg(not(feature = "edge"))]
    let _ = context;
    #[cfg(not(feature = "edge"))]
    Ok(())
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

fn spool_path(context: &Context, body: &Value, suffix: &str) -> Option<std::path::PathBuf> {
    let id = body["spool_id"]
        .as_str()
        .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))?;
    Some(
        context
            .state_root
            .join("data/index-publish")
            .join(format!("{id}.{suffix}")),
    )
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

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
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
    if let Some(code) = store.recovery_error() {
        return error(StatusCode::SERVICE_UNAVAILABLE, code);
    }
    let Ok(bytes) = to_bytes(request.into_body(), 64 * 1024 * 1024).await else {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "INDEX_BATCH_TOO_LARGE");
    };
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID");
    };
    execute(context, body).await
}

pub(crate) async fn execute(context: Arc<Context>, body: Value) -> Response {
    let Some(store) = context.index_store.as_ref() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "INDEX_STORAGE_UNAVAILABLE");
    };
    if let Some(code) = store.recovery_error() {
        return error(StatusCode::SERVICE_UNAVAILABLE, code);
    }
    #[cfg(feature = "edge")]
    if body["action"] == "hybrid_search" {
        let mut body = body;
        let Some(edge) = store.edge.get() else {
            return error(StatusCode::NOT_IMPLEMENTED, "EDGE_STORAGE_REQUIRED");
        };
        if collection_name(&body) != Some(edge.collection()) {
            return error(StatusCode::CONFLICT, "INDEX_COLLECTION_MISMATCH");
        }
        let edge = edge.clone();
        let Ok(permit) = context.workspace_slots.clone().try_acquire_owned() else {
            return error(StatusCode::SERVICE_UNAVAILABLE, "EDGE_BUSY");
        };
        return match tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let _permit = permit;
            if !body.get("vector").is_some_and(|v| !v.is_null()) {
                let model = body["model_id"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("EMBEDDING_MODEL_REQUIRED")?;
                let dimension = body["dimension"]
                    .as_u64()
                    .filter(|v| *v > 0)
                    .ok_or("EMBEDDING_DIMENSION_REQUIRED")?;
                let text = body["text"].as_str().ok_or("EDGE_QUERY_INVALID")?;
                // Capture the generation before inference, including cold model startup.
                let revision = edge.generation()?;
                if body.get("revision").is_some_and(|v| v != &revision) {
                    return Err("INDEX_CHANGED_DURING_SEARCH".into());
                }
                let encoded = crate::embedding_worker::encode(
                    &json!({"model_id":model,"dimension":dimension,"texts":[text]}),
                )?;
                body["vector"] = encoded["vectors"][0].clone();
                body["revision"] = revision;
            }
            edge.hybrid_search(&body).map_err(str::to_owned)
        })
        .await
        {
            Ok(Ok(value)) => json_response(StatusCode::OK, value, "rust.retrieval"),
            Ok(Err(code)) => error(StatusCode::CONFLICT, &code),
            Err(_) => error(StatusCode::BAD_GATEWAY, "EDGE_WORKER_FAILED"),
        };
    }
    if body["action"] == "read_sections" {
        return sections::handle(&context, store, &body).await;
    }
    if body["action"] == "prepare_cache" {
        return cache::handle(&context, store, &body).await;
    }
    if matches!(
        body["action"].as_str(),
        Some("publish_file" | "recover_file")
    ) {
        return publication::handle(context, body).await;
    }
    let Some((collection, method, delete, payload)) = mutation(&body) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_BATCH_INVALID");
    };
    if write(
        &context,
        store,
        collection,
        method,
        if delete { "delete" } else { "" },
        payload,
        None,
    )
    .await
    .is_ok()
    {
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
    error(StatusCode::BAD_GATEWAY, "INDEX_STORAGE_WRITE_FAILED")
}

async fn write(
    context: &Context,
    store: &Store,
    collection: &str,
    method: reqwest::Method,
    operation: &str,
    payload: Value,
    expected_count: Option<usize>,
) -> Result<(), &'static str> {
    #[cfg(feature = "edge")]
    if let Some(edge) = store.edge.get() {
        if edge.collection() != collection {
            return Err("INDEX_COLLECTION_MISMATCH");
        }
        let edge = edge.clone();
        let operation = if method == reqwest::Method::PUT {
            "upsert"
        } else if operation == "count" {
            "count"
        } else {
            "delete"
        };
        let value = tokio::task::spawn_blocking(move || edge.call(operation, &payload))
            .await
            .map_err(|_| "INDEX_STORAGE_WRITE_FAILED")??;
        return if expected_count.is_some_and(|n| value["count"].as_u64() != Some(n as u64)) {
            Err("QDRANT_POINT_COUNT_MISMATCH")
        } else {
            Ok(())
        };
    }
    let mut target = store.target.clone();
    {
        let mut segments = target
            .path_segments_mut()
            .expect("HTTP URL has path segments");
        segments
            .pop_if_empty()
            .extend(["collections", collection, "points"]);
        if !operation.is_empty() {
            segments.push(operation);
        }
    }
    if expected_count.is_none() {
        target.query_pairs_mut().append_pair("wait", "true");
    }
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
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if value["status"] == "ok" {
                        if let Some(count) = expected_count {
                            return if value["result"]["count"].as_u64() == Some(count as u64) {
                                Ok(())
                            } else {
                                Err("QDRANT_POINT_COUNT_MISMATCH")
                            };
                        }
                        if value["result"]["status"] == "completed" {
                            return Ok(());
                        }
                    }
                }
            }
        } else {
            eprintln!(
                "{}",
                json!({"event":"index_store_rejected","operation":operation,
                "upstream_status":response.status().as_u16()})
            );
        }
    }
    Err("INDEX_STORAGE_WRITE_FAILED")
}
