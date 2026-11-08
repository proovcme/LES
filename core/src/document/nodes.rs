//! Final node budgets and document-local hierarchy, independent of format workers.
use super::{embedding_chunks, tokenizer};
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use std::sync::Arc;

type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "DOCUMENT_NODES_INVALID";
pub(super) const MAX_NODES: usize = 50_000;
const MAX_BYTES: usize = 512 * 1024 * 1024;

pub(super) fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.),
    }
}
pub(super) fn string(value: &Value) -> String {
    match value {
        Value::String(v) => v.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        value => value.to_string(),
    }
}
pub(super) fn sha1(value: &str) -> String {
    format!("{:x}", Sha1::digest(value.as_bytes()))
}
pub(super) fn defaults(payload: &mut Map<String, Value>, values: Value) {
    for (key, value) in values.as_object().unwrap() {
        payload.entry(key.clone()).or_insert_with(|| value.clone());
    }
}
pub(crate) struct Builder {
    budget: usize,
    minimum: usize,
    unit: String,
    tokenizer: Option<Arc<tokenizer::Local>>,
    nodes: Vec<Value>,
    bytes: usize,
}
impl Builder {
    pub(crate) fn new(config: &Value) -> Result<Self> {
        let budget = config["size"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| *v > 0)
            .ok_or(INVALID)?;
        let minimum = config["minimum"]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(INVALID)?;
        let unit = config["unit"]
            .as_str()
            .filter(|v| matches!(*v, "tokens" | "chars"))
            .ok_or(INVALID)?
            .to_owned();
        let tokenizer = if config["tokenizer"].is_object() {
            Some(tokenizer::load(&config["tokenizer"])?)
        } else {
            None
        };
        if unit == "tokens" && tokenizer.is_none() {
            return Err("DOCUMENT_TOKENIZER_UNAVAILABLE");
        }
        if unit == "chars" && tokenizer.is_some() {
            return Err(INVALID);
        }
        Ok(Self {
            budget,
            minimum,
            unit,
            tokenizer,
            nodes: Vec::new(),
            bytes: 0,
        })
    }
    pub(crate) fn push(&mut self, mut node: Value, ordinal: usize) -> Result<()> {
        if !node.is_object() {
            return Err(INVALID);
        }
        let text = node["text"].as_str().unwrap_or("");
        // A complete physical page may contain only a decisive sheet/equipment
        // code. The generic noise floor must not erase its confirmed body.
        let physical_page = node["payload"]["type"] == "pdf_page_text"
            && node["payload"]["page"].as_u64().is_some_and(|v| v > 0)
            && text
                .split_once("\n\n")
                .is_some_and(|(_, body)| !super::chunks::trim(body).is_empty());
        let minimum = if physical_page { 1 } else { self.minimum };
        let request = json!({"text":text,"size":self.budget,"minimum":minimum});
        let local = self.tokenizer.as_ref();
        let measure = |value: &str| local.unwrap().count(value);
        let prepared = embedding_chunks::execute(
            &request,
            if local.is_some() {
                Some(&measure)
            } else {
                None
            },
        )?;
        let parent = if present(&node["doc_id"]) {
            string(&node["doc_id"])
        } else {
            format!("node-{ordinal}")
        };
        let typing = node.as_object_mut().unwrap().remove("_les_file_typing");
        for part in prepared["parts"].as_array().ok_or(INVALID)? {
            if self.nodes.len() >= MAX_NODES {
                return Err("INDEX_FILE_CHUNK_LIMIT");
            }
            let text = part["text"].as_str().ok_or(INVALID)?;
            let child = part["ord"].as_u64().ok_or(INVALID)?;
            let length = part["length"].as_u64().ok_or(INVALID)?;
            if length > self.budget as u64 {
                return Err("DOCUMENT_CHUNKING_INVALID");
            }
            let mut value = node.clone();
            if !value["payload"].is_object() {
                value["payload"] = json!({});
            }
            let payload = value["payload"].as_object_mut().unwrap();
            let parent_node = payload
                .get("parent_node_id")
                .filter(|v| present(v))
                .cloned()
                .unwrap_or(json!(parent));
            payload.extend(json!({"embedding_chunk_unit":self.unit,"embedding_chunk_length":length,
                "embedding_chunk_budget":self.budget,"embedding_budget_enforced":true,
                "content_sanitized":prepared["quality"]["sanitized"],"content_quality":prepared["quality"],
                "parent_node_id":parent_node,"child_ord":child}).as_object().unwrap().clone());
            if let Some(typing) = typing.as_ref() {
                let typing = typing.as_object().ok_or(INVALID)?;
                if typing.keys().any(|key| {
                    !matches!(
                        key.as_str(),
                        "file_kind"
                            | "content_layers"
                            | "content_layer_labels"
                            | "document_role"
                            | "navigation_terms"
                            | "source_granularity"
                            | "typed_by"
                    )
                }) {
                    return Err(INVALID);
                }
                payload.extend(typing.clone());
            }
            let suffix = sha1(&format!("{parent}\n{child}\n{text}"));
            value["doc_id"] = json!(format!("{parent}:budget:{}", &suffix[..16]));
            value["text"] = json!(text);
            self.bytes += serde_json::to_vec(&value).map_err(|_| INVALID)?.len();
            if self.bytes > MAX_BYTES {
                return Err("INDEX_FILE_CHUNK_LIMIT");
            }
            self.nodes.push(value);
        }
        Ok(())
    }
    pub(crate) fn finish(
        mut self,
        dataset: &str,
        file: &str,
        revision: &str,
        window: usize,
    ) -> Result<Vec<Value>> {
        super::node_hierarchy::context(&mut self.nodes, dataset, file, revision, window.max(1))?;
        Ok(self.nodes)
    }
}

pub(crate) fn execute(request: &Value) -> Result<Value> {
    let mut builder = Builder::new(&request["config"])?;
    for (index, node) in request["nodes"]
        .as_array()
        .ok_or(INVALID)?
        .iter()
        .enumerate()
    {
        builder.push(node.clone(), index)?;
    }
    Ok(json!({"nodes":builder.finish(
        request["dataset_id"].as_str().ok_or(INVALID)?,
        request["file_name"].as_str().ok_or(INVALID)?,
        request["revision"].as_str().unwrap_or(""),
        request["window"].as_u64().unwrap_or(4).max(1) as usize
    )?}))
}
