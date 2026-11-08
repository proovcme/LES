//! Read only the dense vectors of a single registered file and model fingerprint.
use super::{collection_name, sections, spool_path, valid_id, Store};
use crate::{error, json_response, Context};
use axum::{http::StatusCode, response::Response};
use les_core::index_cache::{is_hash, CacheWriter};
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

const INVALID: &str = "INDEX_CACHE_INVALID";
const UNAVAILABLE: &str = "INDEX_CACHE_UNAVAILABLE";

async fn prepare(context: &Context, store: &Store, body: &Value) -> Result<usize, &'static str> {
    let collection = collection_name(body).ok_or(INVALID)?;
    let path = spool_path(context, body, "cache").ok_or(INVALID)?;
    let dimension = body["dimension"]
        .as_u64()
        .filter(|n| (1..=65_536).contains(n))
        .ok_or(INVALID)? as u32;
    let mut must = Vec::new();
    for key in ["dataset_id", "file_name", "embedding_fingerprint"] {
        let value = body[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or(INVALID)?;
        must.push(json!({"key":key,"match":{"value":value}}));
    }
    let dense = match &body["dense"] {
        Value::Null => None,
        Value::String(name) if !name.trim().is_empty() => Some(name.as_str()),
        _ => return Err(INVALID),
    };
    let vector_fields = dense.map_or(json!(true), |name| json!([name]));
    let mut writer = CacheWriter::create(&path, dimension).map_err(|_| UNAVAILABLE)?;
    let mut offset = Value::Null;
    let mut seen = HashSet::new();
    let mut offsets = HashSet::new();
    // At most half a million numeric coordinates in each HTTP response.
    let limit = (500_000 / dimension).clamp(1, 256);
    'pages: loop {
        let mut request = json!({"filter":{"must":must},"limit":limit,
            "with_payload":["dataset_id","file_name","embedding_fingerprint","content_hash"],
            "with_vector":vector_fields});
        if !offset.is_null() {
            request["offset"] = offset;
        }
        let page = sections::request(context, store, collection, true, request)
            .await
            .map_err(|_| UNAVAILABLE)?;
        let points = page["points"]
            .as_array()
            .filter(|points| points.len() <= limit as usize)
            .ok_or(UNAVAILABLE)?;
        for point in points {
            let payload = &point["payload"];
            if ["dataset_id", "file_name", "embedding_fingerprint"]
                .iter()
                .any(|&key| payload[key] != body[key])
            {
                return Err(UNAVAILABLE);
            }
            let Some(hash) = payload["content_hash"]
                .as_str()
                .filter(|value| is_hash(value))
            else {
                continue;
            };
            if seen.contains(hash) {
                continue;
            }
            let vector = dense.map_or(&point["vector"], |name| &point["vector"][name]);
            let Some(vector) = vector
                .as_array()
                .filter(|items| items.len() == dimension as usize)
            else {
                continue;
            };
            let Some(vector) = vector
                .iter()
                .map(|value| value.as_f64().map(|n| n as f32).filter(|n| n.is_finite()))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            if !writer.add(hash, &vector).map_err(|_| UNAVAILABLE)? {
                break 'pages;
            }
            seen.insert(hash.to_owned());
        }
        offset = page["next_page_offset"].clone();
        if offset.is_null() {
            break;
        }
        if points.is_empty()
            || (!valid_id(&offset) && offset.as_u64().is_none())
            || !offsets.insert(offset.to_string())
        {
            return Err(UNAVAILABLE);
        }
    }
    writer.finish().map_err(|_| UNAVAILABLE)?;
    Ok(seen.len())
}

pub(super) async fn handle(context: &Context, store: &Store, body: &Value) -> Response {
    match tokio::time::timeout(Duration::from_secs(30), prepare(context, store, body)).await {
        Ok(Ok(cached)) => json_response(
            StatusCode::OK,
            json!({"status":"completed","cached":cached}),
            "rust.index_store",
        ),
        Ok(Err(INVALID)) => error(StatusCode::UNPROCESSABLE_ENTITY, INVALID),
        _ => error(StatusCode::BAD_GATEWAY, UNAVAILABLE),
    }
}
