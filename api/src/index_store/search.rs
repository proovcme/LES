//! Shared owned-storage reads and Server/Edge hybrid retrieval for native consumers.
use super::Store;
use crate::Context;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
type Result<T> = std::result::Result<T, String>;
impl Store {
    pub(crate) async fn read_owned(
        &self,
        context: &Context,
        collection: &str,
        operation: &str,
        body: Value,
    ) -> Result<Value> {
        if let Some(code) = self.recovery_error() {
            return Err(code.into());
        }
        if !matches!(operation, "scroll" | "retrieve" | "query" | "count") {
            return Err("INDEX_READ_OPERATION_INVALID".into());
        }
        #[cfg(feature = "edge")]
        if let Some(edge) = self.edge.get() {
            if edge.collection() != collection {
                return Err("INDEX_COLLECTION_MISMATCH".into());
            }
            let edge = edge.clone();
            let operation = operation.to_owned();
            return tokio::task::spawn_blocking(move || {
                edge.call(&operation, &body).map_err(str::to_owned)
            })
            .await
            .map_err(|_| "INDEX_READ_UNAVAILABLE".to_owned())?;
        }
        let mut url = self.target.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| "INDEX_READ_UNAVAILABLE")?;
            path.pop_if_empty()
                .extend(["collections", collection, "points"]);
            if operation != "retrieve" {
                path.push(operation);
            }
        }
        let mut reply = context
            .client
            .post(url)
            .header("api-key", &self.key)
            .header("content-type", "application/json")
            .body(body.to_string())
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| "INDEX_READ_UNAVAILABLE")?;
        if !reply.status().is_success() {
            return Err("INDEX_READ_UNAVAILABLE".into());
        }
        let mut bytes = Vec::new();
        while let Some(part) = reply.chunk().await.map_err(|_| "INDEX_READ_UNAVAILABLE")? {
            if bytes.len() + part.len() > 24 * 1024 * 1024 {
                return Err("INDEX_READ_LIMIT".into());
            }
            bytes.extend(part);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "INDEX_READ_INVALID")?;
        if value["status"] != "ok" {
            return Err("INDEX_READ_UNAVAILABLE".into());
        }
        Ok(value["result"].clone())
    }
    pub(crate) async fn hybrid_owned(&self, context: &Context, request: Value) -> Result<Value> {
        if let Some(code) = self.recovery_error() {
            return Err(code.into());
        }
        #[cfg(feature = "edge")]
        if let Some(edge) = self.edge.get() {
            if request["collection"] != edge.collection() {
                return Err("INDEX_COLLECTION_MISMATCH".into());
            }
            let edge = edge.clone();
            return tokio::task::spawn_blocking(move || {
                edge.hybrid_search(&request).map_err(str::to_owned)
            })
            .await
            .map_err(|_| "INDEX_READ_UNAVAILABLE".to_owned())?;
        }
        let collection = request["collection"]
            .as_str()
            .ok_or("INDEX_COLLECTION_MISMATCH")?;
        let revision = self.revision(context, collection).await?;
        let limit = request["limit"]
            .as_u64()
            .filter(|v| (1..=512).contains(v))
            .ok_or("INDEX_READ_LIMIT")? as usize;
        let mut filters = request["filters"].clone();
        filters["node_roles"] = json!(["evidence", ""]);
        let global = self
            .server_leg(context, &request, &filters, &revision, limit)
            .await?;
        let mut points = global;
        if request["hierarchy"] == true {
            filters["node_roles"] = json!(["navigation"]);
            let navigation = self
                .server_leg(
                    context,
                    &request,
                    &filters,
                    &revision,
                    (limit / 2).clamp(4, 16),
                )
                .await?;
            let ids: Vec<_> = navigation
                .iter()
                .filter_map(|p| p["payload"]["node_id"].as_str())
                .filter(|s| !s.is_empty())
                .collect();
            if !ids.is_empty() {
                filters["node_roles"] = json!(["evidence", ""]);
                filters["ancestor_ids"] = json!(ids);
                let children = self
                    .server_leg(context, &request, &filters, &revision, limit)
                    .await?;
                points = fuse(&[points, children], limit, 61., true)?;
            }
        }
        points.retain(|p| p["payload"]["text"].as_str().is_some_and(|s| !s.is_empty()));
        if self.revision(context, collection).await? != revision {
            return Err("INDEX_CHANGED_DURING_SEARCH".into());
        }
        Ok(
            json!({"points":points,"revision":revision,"channels":["dense","bm25_postings"],"hierarchy":request["hierarchy"]==true}),
        )
    }
    async fn server_leg(
        &self,
        context: &Context,
        request: &Value,
        filters: &Value,
        revision: &Value,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let collection = request["collection"]
            .as_str()
            .ok_or("INDEX_COLLECTION_MISMATCH")?;
        let prefetch = (limit * 2).max(24);
        let filter = query_filter(filters);
        let dense=self.read_owned(context,collection,"query",json!({"query":request["vector"],"using":request["dense_name"],"filter":filter,"limit":prefetch,"with_payload":true})).await?;
        let dense = dense["points"]
            .as_array()
            .cloned()
            .ok_or("INDEX_READ_INVALID")?;
        let database = self
            .content_root(context)
            .join(".index-replacements")
            .join(&les_core::sha256_hex(collection.as_bytes())[..24])
            .join("bm25.sqlite");
        let text = request["text"]
            .as_str()
            .ok_or("INDEX_QUERY_INVALID")?
            .to_owned();
        let filters = filters.clone();
        let revision = revision.clone();
        let lexical=tokio::task::spawn_blocking(move||->Result<Value>{
            let tokens=les_core::lexical::current(&[text])?;
            let terms=tokens["documents"][0]["terms"].as_object().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?.keys().cloned().collect::<Vec<_>>();
            if terms.is_empty(){return Ok(json!({"hits":[]}));}
            Ok(les_core::bm25::execute(&json!({"action":"search","database":database,"revision":revision,"terms":terms,"limit":prefetch,
                "dataset_ids":filters["dataset_ids"],"doc_filter":filters["doc_filter"],"node_roles":filters["node_roles"],"ancestor_ids":filters["ancestor_ids"]}))?)
        }).await.map_err(|_|"INDEX_SPARSE_OWNER_FAILED")??;
        let hits = lexical["hits"]
            .as_array()
            .ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
        let ids = hits
            .iter()
            .map(|hit| {
                serde_json::from_str::<Value>(
                    hit[0].as_str().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?,
                )
                .map_err(|_| "INDEX_SPARSE_CONTRACT_INVALID")
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut lexical_points = Vec::new();
        if !ids.is_empty() {
            let read = self
                .read_owned(
                    context,
                    collection,
                    "retrieve",
                    json!({"ids":ids,"with_payload":true,"with_vector":false}),
                )
                .await?;
            let rows = read.as_array().ok_or("INDEX_READ_INVALID")?;
            let mut map: HashMap<_, _> = rows
                .iter()
                .map(|p| (p["id"].to_string(), p.clone()))
                .collect();
            if map.len() != ids.len() {
                return Err("INDEX_CHANGED_DURING_SEARCH".into());
            }
            for (id, hit) in ids.iter().zip(hits) {
                let mut point = map
                    .remove(&id.to_string())
                    .ok_or("INDEX_CHANGED_DURING_SEARCH")?;
                point["score"] = hit[1].clone();
                lexical_points.push(point);
            }
        }
        fuse(
            &[dense, lexical_points],
            limit,
            request["rank_constant"].as_f64().unwrap_or(2.),
            false,
        )
    }
}
fn query_filter(filters: &Value) -> Value {
    let mut must = Vec::new();
    for (name, key) in [
        ("dataset_ids", "dataset_id"),
        ("doc_filter", "file_name"),
        ("node_roles", "node_role"),
        ("ancestor_ids", "ancestor_ids"),
    ] {
        if let Some(values) = filters[name].as_array().filter(|v| !v.is_empty()) {
            let condition = json!({"key":key,"match":{"any":values}});
            must.push(if name == "node_roles" && values.iter().any(|v| v == "") {
                json!({"should":[condition,{"is_empty":{"key":"node_role"}}]})
            } else {
                condition
            });
        }
    }
    let mut result = json!({"must":must});
    if filters["node_roles"]
        .as_array()
        .is_some_and(|v| v.iter().any(|v| v == "evidence") && !v.iter().any(|v| v == "navigation"))
    {
        result["must_not"] = json!([{"key":"evidence_admissible","match":{"value":false}}]);
    }
    result
}
fn fuse(
    channels: &[Vec<Value>],
    limit: usize,
    constant: f64,
    hierarchy: bool,
) -> Result<Vec<Value>> {
    let mut records = HashMap::new();
    let mut ranked = Vec::new();
    for channel in channels {
        let mut ids = Vec::new();
        for point in channel {
            if hierarchy && point["payload"]["node_role"] == "navigation" {
                continue;
            }
            let payload = &point["payload"];
            let id = if hierarchy {
                ["node_id", "doc_id"]
                    .iter()
                    .find_map(|key| payload[*key].as_str().filter(|v| !v.is_empty()))
                    .map(|s| json!(s))
                    .unwrap_or_else(|| {
                        json!(format!(
                            "{}:{}",
                            payload["file_name"].as_str().unwrap_or(""),
                            payload["text"].as_str().unwrap_or("")
                        ))
                    })
            } else {
                point["id"].clone()
            };
            let key = id.to_string();
            if hierarchy {
                records.insert(key, point.clone());
            } else {
                records.entry(key).or_insert_with(|| point.clone());
            }
            ids.push(id);
        }
        ranked.push(json!(ids));
    }
    let ranked = les_core::bm25::execute(
        &json!({"action":"fuse","channels":ranked,"limit":limit,"rank_constant":constant}),
    )?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for hit in ranked["hits"].as_array().ok_or("INDEX_READ_INVALID")? {
        let key = hit[0].to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        let mut row = records.remove(&key).ok_or("INDEX_READ_INVALID")?;
        row["score"] = hit[1].clone();
        rows.push(row);
    }
    Ok(rows)
}
