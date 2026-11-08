//! Request-owned native chat. No request or model/tool callback is delegated to Python.
pub mod commands;
mod error;
pub mod http;
mod journal;
mod model;
pub(crate) mod prepare;
mod runner;
mod tools;
use crate::Context;
use error::Result;
pub use journal::Service;
use serde_json::Value;
#[derive(Clone)]
pub struct Sink {
    sender: tokio::sync::mpsc::Sender<Value>,
}
impl Sink {
    pub async fn emit(&self, event: &str, data: Value) -> Result<()> {
        self.sender
            .send(serde_json::json!({"event":event,"data":data}))
            .await
            .map_err(|_| "CHAT_CANCELLED".into())
    }
    pub async fn progress(&self, stage: &str, label: &str) -> Result<()> {
        self.emit("progress", serde_json::json!({"stage":stage,"label":label}))
            .await
    }
}
pub(crate) fn unwrap(value: Value) -> Result<Value> {
    if value["status"] == "completed" {
        Ok(value["value"].clone())
    } else {
        Err(value["code"]
            .as_str()
            .unwrap_or("CHAT_STORAGE_UNAVAILABLE")
            .into())
    }
}
