//! Owned storage operations for first-index setup. No caller-supplied addresses.
use super::Store;
use crate::Context;
use reqwest::Method;
use serde_json::{json, Value};
#[cfg(feature = "edge")]
use std::sync::Arc;
use std::time::Duration;
impl Store {
    pub(crate) async fn recover_setup(
        &self,
        context: &Context,
        collection: &str,
    ) -> Result<(), &'static str> {
        let value =
            super::publication::recover_file(context, self, &json!({"collection":collection}))
                .await?;
        if value["status"] == "legacy" {
            return Err("INDEX_RECOVERY_INCOMPLETE");
        }
        Ok(())
    }
    pub(crate) async fn setup_call(
        &self,
        context: &Context,
        collection: &str,
        method: Method,
        suffix: &str,
        body: Value,
    ) -> Result<Option<Value>, &'static str> {
        #[cfg(feature = "edge")]
        if let Some(edge) = self.edge.get() {
            if edge.collection() != collection {
                return Err("INDEX_COLLECTION_MISMATCH");
            }
            let action = match (method.as_str(), suffix) {
                ("GET", "") => "collection_info",
                ("POST", "points/count") => "count",
                ("PUT", "index") => "create_payload_index",
                _ => return Err("INDEX_SCHEMA_MIGRATION_REQUIRED"),
            };
            return edge.call(action, &body).map(Some);
        }
        #[cfg(feature = "edge")]
        if self.edge_root.is_some() {
            return Ok(None);
        }
        let mut url = self.target.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| "INDEX_SERVICE_UNAVAILABLE")?;
            segments.pop_if_empty().extend(["collections", collection]);
            if !suffix.is_empty() {
                segments.extend(suffix.split('/'));
            }
        }
        if suffix == "index" {
            url.query_pairs_mut().append_pair("wait", "false");
        }
        let get = method == Method::GET;
        let mut request = context
            .client
            .request(method, url)
            .header("api-key", &self.key)
            .timeout(Duration::from_secs(30));
        if !get {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "INDEX_SERVICE_UNAVAILABLE")?;
        if get && response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err("INDEX_SERVICE_UNAVAILABLE");
        }
        let mut bytes = Vec::new();
        while let Some(part) = response
            .chunk()
            .await
            .map_err(|_| "INDEX_SERVICE_UNAVAILABLE")?
        {
            if bytes.len() + part.len() > 1024 * 1024 {
                return Err("INDEX_STORAGE_RESPONSE_INVALID");
            }
            bytes.extend(part);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "INDEX_STORAGE_RESPONSE_INVALID")?;
        if value["status"] != "ok" {
            return Err("INDEX_STORAGE_RESPONSE_INVALID");
        }
        Ok(Some(value["result"].clone()))
    }
    pub(crate) fn setup_edge(&self, contract: &Value) -> Result<bool, &'static str> {
        #[cfg(feature = "edge")]
        if let Some(root) = &self.edge_root {
            if self.edge.get().is_none() {
                let edge = les_vector::initialize::create(root, contract)?;
                self.edge
                    .set(Arc::new(edge))
                    .map_err(|_| "EDGE_STORAGE_BUSY")?;
                return Ok(true);
            }
        }
        let _ = contract;
        Ok(false)
    }
    pub(crate) fn finish_setup_held(
        &self,
        context: &Context,
        collection: &str,
        fingerprint: &str,
        writer: &les_core::index_publication::Writer,
    ) -> Result<(), &'static str> {
        #[cfg(feature = "edge")]
        if let Some(edge) = self.edge.get() {
            edge.refresh_postings_held(
                false,
                |value| {
                    eprintln!(
                        "{}",
                        json!({"event":"index_setup_lexical_progress","progress":value})
                    )
                },
                writer,
            )?;
            return Ok(());
        }
        let runtime = tokio::runtime::Handle::current();
        les_core::index_postings::refresh(
            &self.content_root(context),
            collection,
            writer,
            false,
            fingerprint,
            |operation, body| {
                runtime.block_on(super::search::server_read(
                    &context.client,
                    &self.target,
                    &self.key,
                    collection,
                    operation,
                    body,
                ))
            },
            super::lexical_progress,
        )?;
        Ok(())
    }
    pub(crate) fn mark_setup_ready(&self) {
        #[cfg(feature = "edge")]
        if self.edge_root.is_some() && self.edge.get().is_some() && self.recovery.get().is_none() {
            let _ = self.recovery.set(Ok(()));
        }
    }
}
