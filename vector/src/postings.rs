//! Incremental BM25 publication under the vector/document generation journal.
use crate::{Result, Shard};
use les_core::index_publication::Writer;
use serde_json::{Value, json};
use std::path::PathBuf;
impl Shard {
    pub fn content_root(&self) -> PathBuf {
        self.root.join("content")
    }
    pub fn journal_directory(&self) -> PathBuf {
        les_core::index_postings::directory(&self.content_root(), self.collection())
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
        self.refresh_postings_held(full, &mut progress, &writer)
    }
    pub fn refresh_postings_held(
        &self,
        full: bool,
        progress: impl FnMut(Value),
        writer: &Writer,
    ) -> Result<Value> {
        let fallback = self.metadata["source_sha256"]
            .as_str()
            .or_else(|| self.metadata["contract_fingerprint"].as_str())
            .unwrap_or("");
        les_core::index_postings::refresh(
            &self.content_root(),
            self.collection(),
            writer,
            full,
            fallback,
            |operation, body| self.call(operation, body),
            progress,
        )
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
