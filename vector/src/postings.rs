//! Incremental BM25 publication under the vector/document generation journal.
use crate::{Result, Shard};
use les_core::index_publication::Writer;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
impl Shard {
    pub fn content_root(&self) -> PathBuf {
        self.root.join("content")
    }
    pub fn journal_directory(&self) -> PathBuf {
        self.content_root()
            .join(".index-replacements")
            .join(&les_core::sha256_hex(self.collection().as_bytes())[..24])
    }
    pub fn generation(&self) -> Result<Value> {
        let value = les_core::index_journal::execute(
            &json!({"database":self.journal_directory().join("journal.sqlite"),"action":"read"}),
        )?["value"]
            .clone();
        if value["phase"] != "idle" {
            return Err("INDEX_RECOVERY_REQUIRED");
        }
        Ok(value["revision"].clone())
    }
    pub fn refresh_postings(&self, full: bool, mut progress: impl FnMut(Value)) -> Result<Value> {
        let writer = Writer::acquire(&self.content_root(), self.collection())?;
        let state = writer.journal("read", json!({}))?;
        let phase = state["phase"].as_str().unwrap_or("idle");
        if !matches!(phase, "idle" | "sparse_refresh" | "sparse_dirty") {
            return Err("INDEX_RECOVERY_REQUIRED");
        }
        let database = self.journal_directory().join("bm25.sqlite");
        let meta = if database.exists() {
            Some(les_core::bm25::execute(&json!({"action":"metadata","database":database}))?["metadata"].clone())
        } else {
            None
        };
        if !full && phase == "idle" && meta.as_ref().is_some_and(|m| m[2] == state["revision"]) {
            return Ok(meta.unwrap());
        }
        let scopes = writer.journal("sparse_changes", json!({}))?;
        let full = full
            || meta.is_none()
            || scopes
                .as_array()
                .is_none_or(|v| v.is_empty() || v.iter().any(Value::is_null));
        let revision = state["revision"]
            .as_str()
            .unwrap_or_else(|| self.metadata["source_sha256"].as_str().unwrap_or(""));
        if revision.is_empty() {
            return Err("INDEX_SPARSE_STALE");
        }
        writer.journal("store_sparse",json!({"value":{"phase":"sparse_refresh","revision":revision,"mode":"bm25","vector_name":"bm25_sparse","owner":"rust.postings.v1"}}))?;
        let handle =
            les_core::bm25::execute(&json!({"action":"begin","database":database}))?["handle"]
                .as_u64()
                .ok_or("INDEX_SPARSE_OWNER_FAILED")?;
        let result = (|| {
            if full {
                les_core::bm25::execute(&json!({"action":"reset","handle":handle}))?;
            }
            let scopes = if full {
                vec![Value::Null]
            } else {
                scopes.as_array().ok_or("INDEX_SPARSE_STALE")?.clone()
            };
            let mut touched = 0;
            for scope in scopes {
                let mut conditions = Vec::new();
                if !scope.is_null() {
                    les_core::bm25::execute(
                        &json!({"action":"clear","handle":handle,"scope":scope}),
                    )?;
                    if let Some(values) = scope["ids"].as_array() {
                        conditions.push(json!({"has_id":values}));
                    } else if let Some(values) = scope["datasets"].as_array() {
                        conditions.push(json!({"key":"dataset_id","match":{"any":values}}));
                    } else {
                        conditions
                            .push(json!({"key":"dataset_id","match":{"value":scope["dataset"]}}));
                    }
                    if scope["file"].is_string() {
                        conditions.push(json!({"key":"file_name","match":{"value":scope["file"]}}));
                    }
                }
                let filter = if conditions.is_empty() {
                    Value::Null
                } else {
                    json!({"must":conditions})
                };
                let mut offset = Value::Null;
                loop {
                    let page=self.call("scroll",&json!({"offset":offset,"filter":filter,"limit":128,"with_payload":["text","dataset_id","file_name","node_role","ancestor_ids"],"with_vector":false}))?;
                    let points = page["points"].as_array().ok_or("EDGE_RESPONSE_INVALID")?;
                    if !points.is_empty() {
                        let texts = points
                            .iter()
                            .map(|p| p["payload"]["text"].as_str().unwrap_or("").to_owned())
                            .collect::<Vec<_>>();
                        let lexical = les_core::lexical::current(&texts)?;
                        let rows=points.iter().zip(lexical["documents"].as_array().ok_or("CORE_LEXICAL_RESPONSE_INVALID")?).map(|(p,t)|json!({"id":p["id"].to_string(),"payload":p["payload"],"terms":t["terms"]})).collect::<Vec<_>>();
                        les_core::bm25::execute(
                            &json!({"action":"put","handle":handle,"points":rows}),
                        )?;
                        touched += points.len();
                        progress(json!({"phase":"lexical","completed":touched,"full":full}));
                    }
                    offset = page["next_page_offset"].clone();
                    if offset.is_null() {
                        break;
                    }
                }
            }
            let count = self.call("count", &json!({"exact":true}))?["count"].clone();
            let meta=les_core::bm25::execute(&json!({"action":"commit","handle":handle,"revision":revision,"expected_count":count}))?["metadata"].clone();
            let n = meta[0].as_f64().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
            let length = meta[1].as_f64().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
            let contract = json!({"schema":"les.sparse-index.v2","mode":"bm25","storage":"sqlite-postings","vector_name":"bm25_sparse","physical_collection":self.collection(),"tokenizer":"les.lexical.v2","idf":"exact-corpus","points":meta[0],"total_length":meta[1],"profile":{"average_length":if n>0.0&&length>0.0{length/n}else{1.0},"k1":1.2,"b":0.75,"schema":"les.bm25.v2"},"revision":revision});
            let path = self.journal_directory().join("sparse.json");
            let pending = path.with_extension("json.pending");
            let mut file =
                File::create(&pending).map_err(|_| "INDEX_SPARSE_CONTRACT_WRITE_FAILED")?;
            file.write_all(contract.to_string().as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|_| "INDEX_SPARSE_CONTRACT_WRITE_FAILED")?;
            drop(file);
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match fs::rename(&pending, &path) {
                    Ok(()) => break,
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return Err("INDEX_SPARSE_CONTRACT_WRITE_FAILED"),
                }
            }
            writer.journal("publish_sparse", json!({"revision":revision}))?;
            Ok(meta)
        })();
        let _ = les_core::bm25::execute(&json!({"action":"rollback","handle":handle}));
        result
    }
    pub fn lexical_search(&self, text: &str, filters: &Value, limit: usize) -> Result<Value> {
        let stamp = self.generation()?;
        let values = les_core::lexical::current(&[text.to_owned()])?;
        let terms = values["documents"][0]["terms"]
            .as_object()
            .ok_or("CORE_LEXICAL_RESPONSE_INVALID")?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut request = json!({"action":"search","database":self.journal_directory().join("bm25.sqlite"),"revision":stamp,"terms":terms,"limit":limit});
        for key in ["dataset_ids", "doc_filter", "node_roles", "ancestor_ids"] {
            request[key] = filters[key].clone();
        }
        let result = les_core::bm25::execute(&request)?;
        if self.generation()? != stamp {
            return Err("INDEX_CHANGED_DURING_SEARCH");
        }
        Ok(result)
    }
}
