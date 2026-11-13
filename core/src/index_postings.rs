//! Shared, generation-bound BM25 maintenance for owned Qdrant storage.
use crate::index_publication::Writer;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, &'static str>;

pub fn directory(content: &Path, collection: &str) -> PathBuf {
    content
        .join(".index-replacements")
        .join(&crate::sha256_hex(collection.as_bytes())[..24])
}

pub fn refresh(
    content: &Path,
    collection: &str,
    writer: &Writer,
    full: bool,
    fallback_revision: &str,
    mut read: impl FnMut(&str, &Value) -> Result<Value>,
    mut progress: impl FnMut(Value),
) -> Result<Value> {
    let state = writer.journal("read", json!({}))?;
    let phase = state["phase"].as_str().unwrap_or("idle");
    if !matches!(phase, "idle" | "sparse_refresh" | "sparse_dirty") {
        return Err("INDEX_RECOVERY_REQUIRED");
    }
    let directory = directory(content, collection);
    let database = directory.join("bm25.sqlite");
    let meta = if database.exists() {
        Some(
            crate::bm25::execute(&json!({"action":"metadata","database":database}))?["metadata"]
                .clone(),
        )
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
    let revision = state["revision"].as_str().unwrap_or(fallback_revision);
    if revision.is_empty() {
        return Ok(Value::Null);
    }
    writer.journal("store_sparse",json!({"value":{"phase":"sparse_refresh","revision":revision,"mode":"bm25","vector_name":"bm25_sparse","owner":"rust.postings.v1"}}))?;
    let handle = crate::bm25::execute(&json!({"action":"begin","database":database}))?["handle"]
        .as_u64()
        .ok_or("INDEX_SPARSE_OWNER_FAILED")?;
    let result = (|| {
        if full {
            crate::bm25::execute(&json!({"action":"reset","handle":handle}))?;
        }
        let empty = full && read("count", &json!({"exact":true}))?["count"] == 0;
        let scopes = if empty {
            Vec::new()
        } else if full {
            vec![Value::Null]
        } else {
            scopes.as_array().ok_or("INDEX_SPARSE_STALE")?.clone()
        };
        let mut touched = 0;
        for scope in scopes {
            let mut conditions = Vec::new();
            if !scope.is_null() {
                crate::bm25::execute(&json!({"action":"clear","handle":handle,"scope":scope}))?;
                if let Some(values) = scope["ids"].as_array() {
                    conditions.push(json!({"has_id":values}));
                } else if let Some(values) = scope["datasets"].as_array() {
                    conditions.push(json!({"key":"dataset_id","match":{"any":values}}));
                } else {
                    conditions.push(json!({"key":"dataset_id","match":{"value":scope["dataset"]}}));
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
                let page = read(
                    "scroll",
                    &json!({"offset":offset,"filter":filter,"limit":128,"with_payload":["text","dataset_id","file_name","node_role","ancestor_ids"],"with_vector":false}),
                )?;
                let points = page["points"].as_array().ok_or("INDEX_READ_INVALID")?;
                if !points.is_empty() {
                    let texts = points
                        .iter()
                        .map(|p| p["payload"]["text"].as_str().unwrap_or("").to_owned())
                        .collect::<Vec<_>>();
                    let lexical = crate::lexical::current(&texts)?;
                    let rows=points.iter().zip(lexical["documents"].as_array().ok_or("CORE_LEXICAL_RESPONSE_INVALID")?).map(|(p,t)|json!({"id":p["id"].to_string(),"payload":p["payload"],"terms":t["terms"]})).collect::<Vec<_>>();
                    crate::bm25::execute(&json!({"action":"put","handle":handle,"points":rows}))?;
                    touched += points.len();
                    progress(json!({"phase":"lexical","completed":touched,"full":full}));
                }
                let next = page["next_page_offset"].clone();
                if !next.is_null() && next == offset {
                    return Err("INDEX_READ_INVALID");
                }
                offset = next;
                if offset.is_null() {
                    break;
                }
            }
        }
        let count = read("count", &json!({"exact":true}))?["count"].clone();
        let meta = crate::bm25::execute(
            &json!({"action":"commit","handle":handle,"revision":revision,"expected_count":count}),
        )?["metadata"]
            .clone();
        let n = meta[0].as_f64().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
        let length = meta[1].as_f64().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
        let contract = json!({"schema":"les.sparse-index.v2","mode":"bm25","storage":"sqlite-postings","vector_name":"bm25_sparse","physical_collection":collection,"tokenizer":"les.lexical.v2","idf":"exact-corpus","points":meta[0],"total_length":meta[1],"profile":{"average_length":if n>0.0&&length>0.0{length/n}else{1.0},"k1":1.2,"b":0.75,"schema":"les.bm25.v2"},"revision":revision});
        let path = directory.join("sparse.json");
        let pending = path.with_extension("json.pending");
        let mut file = File::create(&pending).map_err(|_| "INDEX_SPARSE_CONTRACT_WRITE_FAILED")?;
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
    let _ = crate::bm25::execute(&json!({"action":"rollback","handle":handle}));
    result
}
