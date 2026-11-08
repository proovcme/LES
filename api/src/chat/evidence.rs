//! Source identities and bounded model-facing evidence projections.
use super::Result;
use serde_json::{json, Value};
#[derive(Default)]
pub(super) struct Evidence {
    pub(super) sources: Vec<Value>,
    pub(super) blocks: Vec<Value>,
}
impl Evidence {
    fn key(source: &Value) -> Value {
        json!([
            source["doc_id"],
            source["source_sha256"],
            source["page"],
            source["qdrant_point_id"],
            source["offset"]
        ])
    }
    pub(super) fn add(&mut self, result: &Value) {
        if result["status"] != "ok" {
            return;
        }
        let result = if result["tool"]
            .as_str()
            .is_some_and(|v| v.starts_with("use_extensions_"))
            && result["operation"] == "call"
        {
            &result["result"]
        } else {
            result
        };
        let passages = result["result"]["passages"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                result["result"]["text"]
                    .as_str()
                    .map(|s| vec![json!({"text":s,"source":result["sources"][0]})])
                    .unwrap_or_default()
            });
        for passage in passages {
            let mut source = passage["source"].clone();
            let Some(text) = passage["text"].as_str().filter(|v| !v.is_empty()) else {
                continue;
            };
            if !source.is_object() {
                continue;
            }
            let key = Self::key(&source);
            if self.sources.iter().any(|s| s["native_identity"] == key) {
                continue;
            }
            let index = self.sources.len() + 1;
            source["index"] = json!(index);
            source["native_identity"] = key;
            let name = source["doc_name"].as_str().unwrap_or("Документ");
            let label = source["page"]
                .as_u64()
                .map(|p| format!("{name} · стр. {p}"))
                .unwrap_or_else(|| name.into());
            source["label"] = json!(label);
            source["header"] = json!(format!("[Источник {index} | {label}]"));
            if result["tool"] != "document_review" {
                self.blocks.push(json!(format!(
                    "{}\n{text}",
                    source["header"].as_str().unwrap()
                )));
            }
            self.sources.push(source);
        }
    }
    fn reference(&self, source: &Value) -> Option<Value> {
        self.sources
            .iter()
            .find(|s| s["native_identity"] == Self::key(source))
            .map(|s| json!({"index":s["index"],"label":s["header"]}))
    }
    pub(super) fn review_context(&self, view: &Value) -> Result<Value> {
        Ok(json!(format!(
            "{}\n{}",
            self.review_header(view),
            les_core::tool_execution::canonical_json(view)?
        )))
    }
    pub(super) fn review_header(&self, view: &Value) -> String {
        let read = &view["current_read"];
        self.sources
            .iter()
            .find(|s| {
                s["qdrant_point_id"] == read["fragment_id"]
                    && s["offset"] == read["offset"]
                    && s["dataset_id"] == read["selector"]["dataset_id"]
                    && s["doc_name"] == read["selector"]["file_name"]
                    && s["source_sha256"] == read["source_sha256"]
            })
            .and_then(|s| s["header"].as_str())
            .unwrap_or("")
            .to_owned()
    }
    pub(super) fn projection(&self, payload: &Value) -> Value {
        let mut value = payload.clone();
        if value["tool"]
            .as_str()
            .is_some_and(|v| v.starts_with("use_extensions_"))
            && value["operation"] == "call"
        {
            value["result"] = self.projection(&value["result"]);
        }
        if let Some(rows) = value["result"]["passages"].as_array_mut() {
            for row in rows {
                if let Some(reference) = self.reference(&row["source"]) {
                    row["text"] = reference["label"].clone();
                    row["source"] = reference;
                }
            }
        }
        if value["result"]["text"].is_string() {
            if let Some(reference) = self.reference(&value["sources"][0]) {
                value["result"]["text"] = reference["label"].clone();
            }
        }
        if value["sources"].is_array() {
            value["sources"] = json!(value["sources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| self.reference(s).unwrap_or_else(|| s.clone()))
                .collect::<Vec<_>>());
        }
        if !value["execution"].is_null() {
            value["execution"] =
                json!({"status":value["execution"]["status"],"code":value["execution"]["code"]});
        }
        value
    }
}
