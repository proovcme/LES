//! Explicit copy-only comparison; ordinary launches retain SQLite postings.
use crate::{Result, Shard};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const FIELD: &str = "_les_bm25_tokens_trial_v1";
const MARKER: &str = "edge-bm25-trial.json";

pub(crate) fn mode(request: &Value) -> Result<Option<&'static str>> {
    let env = std::env::var("LES_EXPERIMENT_EDGE_BM25").unwrap_or_default();
    match request["lexical_backend"].as_str().unwrap_or(&env) {
        "" | "sqlite-v1" => Ok(None),
        "matched-v1" => Ok(Some(FIELD)),
        "raw-v1" => Ok(Some("text")),
        _ => Err("EDGE_BM25_TRIAL_MODE_INVALID"),
    }
}
impl Shard {
    pub(crate) fn bm25_trial_marker(&self, ready: bool) -> Result<Value> {
        let path = self.root.join(MARKER);
        let marker: Value =
            serde_json::from_slice(&fs::read(path).map_err(|_| "EDGE_BM25_COPY_REQUIRED")?)
                .map_err(|_| "EDGE_BM25_COPY_INVALID")?;
        let permitted = marker["root"].as_str().ok_or("EDGE_BM25_COPY_INVALID")?;
        if marker["schema"] != "les.edge-bm25-trial.v1"
            || fs::canonicalize(Path::new(permitted)).map_err(|_| "EDGE_BM25_COPY_INVALID")?
                != self.root
            || marker["collection"].as_str() != Some(self.collection())
        {
            return Err("EDGE_BM25_COPY_INVALID");
        }
        if marker["revision"] != self.generation()? {
            return Err("EDGE_BM25_TRIAL_REVISION_CHANGED");
        }
        if ready && marker["phase"] != "ready" {
            return Err("EDGE_BM25_TRIAL_NOT_READY");
        }
        Ok(marker)
    }
    fn save_bm25_trial_marker(&self, marker: &Value) -> Result<()> {
        let pending = self.root.join("edge-bm25-trial.pending.json");
        fs::write(&pending, marker.to_string()).map_err(|_| "EDGE_BM25_TRIAL_WRITE_FAILED")?;
        fs::rename(pending, self.root.join(MARKER)).map_err(|_| "EDGE_BM25_TRIAL_WRITE_FAILED")
    }
    pub(crate) fn bm25_trial_digest(&self) -> Result<Value> {
        self.bm25_trial_marker(false)?;
        let mut offset = Value::Null;
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        loop {
            let page = self.call(
                "scroll",
                &json!({"offset":offset,"limit":64,"with_payload":true,"with_vector":true}),
            )?;
            for point in page["points"].as_array().ok_or("EDGE_RESPONSE_INVALID")? {
                let mut point = point.clone();
                point
                    .as_object_mut()
                    .ok_or("EDGE_RESPONSE_INVALID")?
                    .remove("version");
                if let Some(p) = point["payload"].as_object_mut() {
                    p.remove(FIELD);
                }
                digest.update(serde_json::to_vec(&point).map_err(|_| "EDGE_RESPONSE_INVALID")?);
                digest.update(b"\n");
                count += 1;
            }
            let next = page["next_page_offset"].clone();
            if next.is_null() {
                break;
            }
            if next == offset {
                return Err("EDGE_RESPONSE_INVALID");
            }
            offset = next;
        }
        Ok(json!({"points":count,"source_points_sha256":format!("{:x}",digest.finalize())}))
    }
    pub(crate) fn prepare_bm25_trial(&self) -> Result<Value> {
        let mut marker = self.bm25_trial_marker(false)?;
        let _writer =
            les_core::index_publication::Writer::acquire(&self.content_root(), self.collection())?;
        let before = self.bm25_trial_digest()?;
        if marker
            .get("source_points_sha256")
            .is_some_and(|v| v != &before["source_points_sha256"])
        {
            return Err("EDGE_BM25_TRIAL_SOURCE_CHANGED");
        }
        marker["phase"] = json!("preparing");
        marker["source_points_sha256"] = before["source_points_sha256"].clone();
        self.save_bm25_trial_marker(&marker)?;
        let mut offset = Value::Null;
        let mut completed = 0;
        loop {
            let page = self.call(
                "scroll",
                &json!({"offset":offset,"limit":64,"with_payload":true,"with_vector":false}),
            )?;
            let points = page["points"].as_array().ok_or("EDGE_RESPONSE_INVALID")?;
            let texts = points
                .iter()
                .map(|p| {
                    les_core::search_context::input(
                        &p["payload"],
                        p["payload"]["text"].as_str().unwrap_or(""),
                    )
                })
                .collect::<Vec<_>>();
            let lexical = les_core::lexical::current(&texts)?;
            for (p, terms) in points.iter().zip(
                lexical["documents"]
                    .as_array()
                    .ok_or("CORE_LEXICAL_RESPONSE_INVALID")?,
            ) {
                let mut text = String::new();
                for (term, tf) in terms["terms"]
                    .as_object()
                    .ok_or("CORE_LEXICAL_RESPONSE_INVALID")?
                {
                    for _ in 0..tf.as_u64().ok_or("CORE_LEXICAL_RESPONSE_INVALID")? {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(term);
                    }
                }
                let operation =
                    serde_json::from_value(json!({"points":[p["id"]],"payload":{FIELD:text}}))
                        .map_err(|_| "EDGE_PAYLOAD_INVALID")?;
                self.edge
                    .update(qdrant_edge::UpdateOperation::PayloadOperation(
                        qdrant_edge::PayloadOps::SetPayload(operation),
                    ))
                    .map_err(|_| "EDGE_WRITE_FAILED")?;
                completed += 1;
            }
            // WAL remains enabled; a stopped partial batch is never queryable
            // while the marker is preparing. Sync once per acknowledged page.
            self.flush()?;
            eprintln!(
                "{}",
                json!({"event":"edge_bm25_trial_preparation","completed":completed})
            );
            let next = page["next_page_offset"].clone();
            if next.is_null() {
                break;
            }
            if next == offset {
                return Err("EDGE_RESPONSE_INVALID");
            }
            offset = next;
        }
        for (field, schema) in [
            (
                FIELD,
                json!({"type":"text","tokenizer":"whitespace","lowercase":false,"min_token_len":1,"memory":"cold","scoring":true}),
            ),
            (
                "text",
                json!({"type":"text","tokenizer":"word","lowercase":true,"min_token_len":1,"memory":"cold","scoring":true,
                "stemmer":{"type":"snowball","language":"russian"}}),
            ),
        ] {
            self.call(
                "create_payload_index",
                &json!({"field_name":field,"field_schema":schema}),
            )?;
        }
        self.optimize()?;
        let after = self.bm25_trial_digest()?;
        if before != after {
            return Err("EDGE_BM25_TRIAL_SOURCE_CHANGED");
        }
        marker["phase"] = json!("ready");
        marker["points"] = before["points"].clone();
        marker["analyzer"] = json!(les_core::lexical::LEGACY);
        self.save_bm25_trial_marker(&marker)?;
        Ok(marker)
    }
    pub(crate) fn bm25_trial_lexical(
        &self,
        request: &Value,
        filters: &Value,
        limit: usize,
    ) -> Result<Value> {
        let text = request["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 65536)
            .ok_or("EDGE_QUERY_INVALID")?;
        let Some(field) = mode(request)? else {
            return self.lexical_search(text, filters, limit);
        };
        self.bm25_trial_marker(true)?;
        let question = if field == FIELD {
            les_core::lexical::current(&[text.to_owned()])?["documents"][0]["terms"]
                .as_object()
                .ok_or("CORE_LEXICAL_RESPONSE_INVALID")?
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            text.to_owned()
        };
        if question.is_empty() {
            return Ok(json!({"hits":[]}));
        }
        let mut filter = crate::retrieval::query_filter(filters)?;
        // Match SQLite's lexical candidate domain. Admissibility is checked by
        // the shared retrieval owner after retrieving either engine's IDs.
        if let Some(f) = filter.as_object_mut() {
            f.remove("must_not");
        }
        let query = json!({"query":{"text_query":{"field":field,"text":question}},
            "filter":filter,"limit":limit,"with_payload":true});
        let mut points = if filters["group_size"].is_u64() {
            if filters["group_size"] != 2 {
                return Err("EDGE_BM25_TRIAL_GROUP_INVALID");
            }
            let mut rows = Vec::new();
            for request in les_core::overview::dense_requests(&query, &filters["dataset_ids"])? {
                les_core::overview::merge_dense(
                    &mut rows,
                    &self.call("query_groups", &request)?,
                    limit,
                )?;
            }
            rows
        } else {
            self.call("query", &query)?["points"]
                .as_array()
                .ok_or("EDGE_RESPONSE_INVALID")?
                .clone()
        };
        points.sort_by(|a, b| {
            b["score"]
                .as_f64()
                .unwrap_or(0.)
                .total_cmp(&a["score"].as_f64().unwrap_or(0.))
                .then_with(|| a["id"].to_string().cmp(&b["id"].to_string()))
        });
        points.truncate(limit);
        Ok(
            json!({"hits":points.iter().map(|p|json!([p["id"].to_string(),p["score"]])).collect::<Vec<_>>()}),
        )
    }
}
