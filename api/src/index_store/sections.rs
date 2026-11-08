//! Validate ranked points against Qdrant and read their parent sections.
use super::{collection_name, valid_id, Store};
use crate::{error, json_response, Context};
use axum::{http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

const UNAVAILABLE: &str = "SECTION_READ_UNAVAILABLE";
const CHANGED: &str = "INDEX_CHANGED_DURING_SEARCH";
const SCOPE: &str = "SECTION_SCOPE_MISMATCH";
const LIMIT: &str = "SECTION_READ_LIMIT";

fn fields(value: &Value) -> [&str; 3] {
    ["dataset_id", "file_name", "parent_id"].map(|key| value[key].as_str().unwrap_or_default())
}

fn evidence(value: &Value) -> bool {
    value["node_role"] != "navigation" && value["evidence_admissible"] != false
}

fn id(value: &Value) -> Result<String, &'static str> {
    if valid_id(value) {
        return Ok(value.as_str().unwrap().to_owned());
    }
    value.as_u64().map(|n| n.to_string()).ok_or(CHANGED)
}

pub(super) async fn request(
    context: &Context,
    store: &Store,
    collection: &str,
    scroll: bool,
    body: Value,
) -> Result<Value, &'static str> {
    #[cfg(feature = "edge")]
    if let Some(edge) = store.edge.get() {
        if edge.collection() != collection {
            return Err(SCOPE);
        }
        let edge = edge.clone();
        let action = if scroll { "scroll" } else { "retrieve" };
        let value = tokio::task::spawn_blocking(move || edge.call(action, &body))
            .await
            .map_err(|_| UNAVAILABLE)?
            .map_err(|_| UNAVAILABLE)?;
        if value.to_string().len() > 24 * 1024 * 1024 {
            return Err(LIMIT);
        }
        return Ok(value);
    }
    let mut url = store.target.clone();
    {
        let mut path = url.path_segments_mut().map_err(|_| UNAVAILABLE)?;
        path.pop_if_empty()
            .extend(["collections", collection, "points"]);
        if scroll {
            path.push("scroll");
        }
    }
    let mut response = context
        .client
        .post(url)
        .header("api-key", &store.key)
        .header("content-type", "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(12))
        .send()
        .await
        .map_err(|_| UNAVAILABLE)?;
    if !response.status().is_success() {
        return Err(UNAVAILABLE);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
        if bytes.len() + chunk.len() > 24 * 1024 * 1024 {
            return Err(LIMIT);
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| UNAVAILABLE)?;
    if value["status"] != "ok" {
        return Err(UNAVAILABLE);
    }
    Ok(value["result"].clone())
}

pub(crate) async fn read(
    context: &Context,
    store: &Store,
    body: &Value,
) -> Result<Value, &'static str> {
    let collection = collection_name(body).ok_or(SCOPE)?;
    let revision = store.revision(context, collection).await?;
    if body.get("revision").is_some_and(|v| v != &revision) {
        return Err(CHANGED);
    }
    let seeds = body["seeds"]
        .as_array()
        .filter(|items| !items.is_empty() && items.len() <= 50)
        .ok_or(SCOPE)?;
    let scopes = body["scopes"]
        .as_array()
        .filter(|items| items.len() <= 8)
        .ok_or(SCOPE)?;
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for seed in seeds {
        if seed.get("_index_revision").is_some_and(|v| v != &revision) {
            return Err(CHANGED);
        }
        let key = seed["id"].as_str().ok_or(SCOPE)?;
        let id = if valid_id(&seed["id"]) {
            json!(key)
        } else {
            json!(key.parse::<u64>().map_err(|_| SCOPE)?)
        };
        if seen.insert(key) {
            ids.push(id);
        }
    }
    let rows = request(
        context,
        store,
        collection,
        false,
        json!({
            "ids":ids,"with_payload":true,"with_vector":false
        }),
    )
    .await?;
    let mut current = HashMap::new();
    for row in rows.as_array().ok_or(UNAVAILABLE)? {
        let key = id(&row["id"])?;
        if !seen.contains(key.as_str()) || current.insert(key, &row["payload"]).is_some() {
            return Err(CHANGED);
        }
    }
    for seed in seeds {
        let payload = current
            .get(seed["id"].as_str().ok_or(SCOPE)?)
            .ok_or(CHANGED)?;
        if !evidence(payload)
            || fields(seed) != fields(payload)
            || [
                "text",
                "page",
                "source_page",
                "source_ref",
                "chunk_ord",
                "source_sha256",
            ]
            .into_iter()
            .any(|key| seed[key] != payload[key])
        {
            return Err(CHANGED);
        }
    }
    let mut sections = Vec::new();
    let mut seen_scopes = HashSet::new();
    let mut bytes = 0usize;
    for scope in scopes {
        let key = fields(scope);
        if key.iter().any(|part| part.is_empty())
            || !seen_scopes.insert(key)
            || !seeds.iter().any(|seed| fields(seed) == key)
        {
            return Err(SCOPE);
        }
        let hits: HashSet<_> = seeds
            .iter()
            .filter(|seed| fields(seed) == key)
            .filter_map(|seed| seed["id"].as_str())
            .collect();
        let must = ["dataset_id", "file_name", "parent_id"]
            .into_iter()
            .zip(key)
            .map(|(field, value)| json!({"key":field,"match":{"value":value}}))
            .collect::<Vec<_>>();
        let filter = json!({
            "must": must,
            "must_not":[{"key":"node_role","match":{"value":"navigation"}},
                {"key":"evidence_admissible","match":{"value":false}}]
        });
        let mut offset = Value::Null;
        let mut offsets = HashSet::new();
        let mut points = Vec::new();
        let mut point_ids = HashSet::new();
        let mut chars = 0usize;
        loop {
            let mut query =
                json!({"filter":filter,"limit":128,"with_payload":true,"with_vector":false});
            if !offset.is_null() {
                query["offset"] = offset;
            }
            let page = request(context, store, collection, true, query).await?;
            let rows = page["points"].as_array().ok_or(UNAVAILABLE)?;
            if rows.len() > 128 || points.len() + rows.len() > 2048 {
                return Err(LIMIT);
            }
            for row in rows {
                let payload = &row["payload"];
                if fields(payload) != key || !evidence(payload) {
                    return Err(SCOPE);
                }
                if seeds.iter().filter(|seed| fields(seed) == key).any(|seed| {
                    seed["source_sha256"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                        && seed["source_sha256"] != payload["source_sha256"]
                }) {
                    return Err(CHANGED);
                }
                let id = id(&row["id"])?;
                if !point_ids.insert(id.clone()) {
                    return Err(CHANGED);
                }
                chars += payload["text"].as_str().unwrap_or_default().chars().count();
                let point = json!({"id":id,"payload":payload});
                bytes += point.to_string().len();
                if chars > 2_000_000 || bytes > 64 * 1024 * 1024 {
                    return Err(LIMIT);
                }
                points.push(point);
            }
            offset = page["next_page_offset"].clone();
            if offset.is_null() {
                break;
            }
            if rows.is_empty() || !offsets.insert(id(&offset)?) {
                return Err(CHANGED);
            }
        }
        if !hits.iter().all(|hit| point_ids.contains(*hit)) {
            return Err(CHANGED);
        }
        sections.push(json!({"scope":scope,"points":points}));
    }
    if store.revision(context, collection).await? != revision {
        return Err(CHANGED);
    }
    Ok(json!({"current":current,"sections":sections}))
}

pub(super) async fn handle(context: &Context, store: &Store, body: &Value) -> Response {
    match tokio::time::timeout(Duration::from_secs(14), read(context, store, body)).await {
        Ok(Ok(value)) => json_response(StatusCode::OK, value, "rust.index_sections"),
        Ok(Err(code @ (CHANGED | SCOPE))) => error(StatusCode::CONFLICT, code),
        Ok(Err(LIMIT)) => error(StatusCode::UNPROCESSABLE_ENTITY, LIMIT),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE),
    }
}
