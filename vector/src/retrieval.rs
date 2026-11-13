//! Dense, exact BM25 and hierarchy share one checked document generation.
use crate::{Result, Shard};
use les_core::retrieval_fusion::retrieve as fuse;
use serde_json::{Value, json};
use std::collections::HashMap;

fn query_filter(filters: &Value) -> Result<Value> {
    let mut must = Vec::new();
    for (key, field) in [
        ("dataset_ids", "dataset_id"),
        ("doc_filter", "file_name"),
        ("node_roles", "node_role"),
        ("ancestor_ids", "ancestor_ids"),
    ] {
        if filters[key].is_null() {
            continue;
        }
        let values = filters[key]
            .as_array()
            .filter(|v| v.len() <= 512)
            .ok_or("EDGE_FILTER_INVALID")?;
        if !values.iter().all(|v| {
            v.as_str()
                .is_some_and(|s| (!s.is_empty() || key == "node_roles") && s.len() <= 4096)
        }) {
            return Err("EDGE_FILTER_INVALID");
        }
        if !values.is_empty() {
            let condition = json!({"key":field,"match":{"any":values}});
            must.push(if key == "node_roles" && values.iter().any(|v| v == "") {
                json!({"should":[condition,{"is_empty":{"key":"node_role"}}]})
            } else {
                condition
            });
        }
    }
    Ok(if must.is_empty() {
        Value::Null
    } else {
        let mut filter = json!({"must":must});
        if filters["node_roles"].as_array().is_some_and(|v| {
            v.iter().any(|v| v == "evidence") && !v.iter().any(|v| v == "navigation")
        }) {
            filter["must_not"] = json!([{"key":"evidence_admissible","match":{"value":false}}]);
        }
        filter
    })
}
impl Shard {
    fn hybrid_leg(&self, request: &Value, filters: &Value, limit: usize) -> Result<Vec<Value>> {
        let filter = query_filter(filters)?;
        let text = request["text"].as_str().ok_or("EDGE_QUERY_INVALID")?;
        let values = les_core::lexical::current(&[text.to_owned()])?;
        let lexical = !values["documents"][0]["terms"]
            .as_object()
            .ok_or("CORE_LEXICAL_RESPONSE_INVALID")?
            .is_empty();
        let prefetch = if lexical { (limit * 2).max(24) } else { limit };
        let dense = self.call(
            "query",
            &json!({"query":request["vector"],"using":request["dense_name"],
            "filter":filter,"limit":prefetch,"with_payload":true}),
        )?;
        let dense = dense["points"]
            .as_array()
            .ok_or("EDGE_RESPONSE_INVALID")?
            .clone();
        if !lexical {
            return Ok(dense);
        }
        let hits = self.lexical_search(text, filters, prefetch)?;
        let hits = hits["hits"].as_array().ok_or("EDGE_RESPONSE_INVALID")?;
        let mut lexical = Vec::new();
        if !hits.is_empty() {
            let ids = hits
                .iter()
                .map(|h| {
                    serde_json::from_str::<Value>(
                        h[0].as_str().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?,
                    )
                    .map_err(|_| "INDEX_SPARSE_CONTRACT_INVALID")
                })
                .collect::<Result<Vec<_>>>()?;
            let records = self.call(
                "retrieve",
                &json!({"ids":ids,"with_payload":true,"with_vector":false}),
            )?;
            let records = records
                .as_array()
                .ok_or("EDGE_RESPONSE_INVALID")?
                .iter()
                .map(|p| (p["id"].to_string(), p))
                .collect::<HashMap<_, _>>();
            if records.len() != ids.len() {
                return Err("INDEX_CHANGED_DURING_SEARCH");
            }
            for (id, hit) in ids.iter().zip(hits) {
                let mut row = (*records
                    .get(&id.to_string())
                    .ok_or("INDEX_CHANGED_DURING_SEARCH")?)
                .clone();
                if filters["node_roles"].as_array().is_some_and(|v| {
                    v.iter().any(|v| v == "evidence") && !v.iter().any(|v| v == "navigation")
                }) && row["payload"]["evidence_admissible"] == false
                {
                    continue;
                }
                row["score"] = hit[1].clone();
                lexical.push(row);
            }
        }
        fuse(
            &[dense, lexical],
            limit,
            request["rank_constant"].as_f64().unwrap_or(2.),
            request["fusion"].as_str().unwrap_or("rrf"),
            request["preserve_heads"] == true,
        )
    }
    pub fn hybrid_search(&self, request: &Value) -> Result<Value> {
        request["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 65536)
            .ok_or("EDGE_QUERY_INVALID")?;
        let limit = request["limit"]
            .as_u64()
            .filter(|v| (1..=512).contains(v))
            .ok_or("EDGE_LIMIT_INVALID")? as usize;
        let constant = request["rank_constant"].as_f64().unwrap_or(2.);
        if !constant.is_finite() || constant <= 0. {
            return Err("EDGE_QUERY_INVALID");
        }
        self.refresh_postings(false, |_| {})?;
        let revision = self.generation()?;
        if request.get("revision").is_some_and(|v| v != &revision) {
            return Err("INDEX_CHANGED_DURING_SEARCH");
        }
        let mut filters = request["filters"].as_object().cloned().unwrap_or_default();
        let hierarchy = request["hierarchy"] == true;
        filters.insert("node_roles".into(), json!(["evidence", ""]));
        let global = self.hybrid_leg(request, &json!(filters), limit)?;
        let mut points = global;
        let mut hierarchy_candidates = Vec::new();
        if hierarchy {
            filters.insert("node_roles".into(), json!(["navigation"]));
            let routes = self.hybrid_leg(request, &json!(filters), (limit / 2).clamp(4, 16))?;
            let ids = routes
                .iter()
                .filter_map(|p| p["payload"]["node_id"].as_str())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                filters.insert("node_roles".into(), json!(["evidence", ""]));
                filters.insert("ancestor_ids".into(), json!(ids));
                let descendants = self.hybrid_leg(request, &json!(filters), limit)?;
                hierarchy_candidates =
                    les_core::bm25::hierarchy_candidates(&points, descendants, limit);
            }
        }
        points.retain(|p| p["payload"]["text"].as_str().is_some_and(|s| !s.is_empty()));
        if self.generation()? != revision {
            return Err("INDEX_CHANGED_DURING_SEARCH");
        }
        Ok(
            json!({"points":points,"revision":revision,"channels":["dense","bm25_postings"],"fusion":request["fusion"].as_str().unwrap_or("rrf"),"hierarchy":hierarchy,"hierarchy_candidates":hierarchy_candidates}),
        )
    }
}
