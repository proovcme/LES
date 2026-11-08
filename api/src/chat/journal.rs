use super::{Context, Result};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
};
pub struct Service {
    _lease: File,
    pub local: std::sync::Arc<tokio::sync::Semaphore>,
    pub remote: std::sync::Arc<tokio::sync::Semaphore>,
    pub summary: crate::conversation::Scheduler,
}
impl Service {
    pub fn new(context: &Context) -> Result<Self> {
        let parent = context
            .database
            .parent()
            .ok_or("CHAT_STORAGE_UNAVAILABLE")?;
        std::fs::create_dir_all(parent).map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
        let name = context
            .database
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("CHAT_STORAGE_UNAVAILABLE")?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join(format!("{name}.chat-owner.lock")))
            .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
        lease.try_lock().map_err(|_| "CHAT_SERVICE_BUSY")?;
        les_core::chat_request::journal::recover(&context.database)?;
        les_core::conversation_summary::queue::execute(&context.database, "recover", &json!({}))?;
        Ok(Self {
            _lease: lease,
            local: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            remote: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
            summary: crate::conversation::Scheduler::new(),
        })
    }
}
pub struct Journal {
    pub id: String,
    pub history: i64,
    database: PathBuf,
    completed: bool,
}
impl Journal {
    pub fn new(database: PathBuf, value: &Value) -> Result<Self> {
        Ok(Self {
            id: value["id"]
                .as_str()
                .ok_or("CHAT_STORAGE_UNAVAILABLE")?
                .into(),
            history: value["history_id"]
                .as_i64()
                .ok_or("CHAT_STORAGE_UNAVAILABLE")?,
            database,
            completed: false,
        })
    }
    pub async fn checkpoint(&self, event: &str, data: Value) -> Result<()> {
        let database = self.database.clone();
        let history = self.history;
        let id = self.id.clone();
        let event = event.to_owned();
        tokio::task::spawn_blocking(move || {
            les_core::chat_request::journal::checkpoint(
                &database,
                &id,
                history,
                &event,
                data.as_str().ok_or("CHAT_REQUEST_INVALID")?,
            )
            .map_err(Into::into)
        })
        .await
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?
    }
    pub async fn fail(&mut self, error: &Value) -> Result<()> {
        let db = self.database.clone();
        let id = self.id.clone();
        let history = self.history;
        let error = error.clone();
        tokio::task::spawn_blocking(move || {
            les_core::chat_request::journal::interrupt(&db, &id, history, Some(&error))
        })
        .await
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")??;
        self.completed = true;
        Ok(())
    }
    pub async fn finish(&mut self, request: &Value, response: &Value) -> Result<()> {
        let database = self.database.clone();
        let history = self.history;
        let id = self.id.clone();
        let request = request.clone();
        let response = response.clone();
        tokio::task::spawn_blocking(move||->Result<()>{
            let mut row=json!({"question":request["question"],"answer":response["answer"],"sources":response["sources"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
                "crag_status":response["crag_status"].as_str().unwrap_or("UNVALIDATED"),"latency_sec":response["latency_sec"],"tokens":response["tokens"],"session_id":request["session_id"],
                "route_channel":"generic","route_reason":"native_chat","requested_dataset_filter":request["dataset_filter"].as_str().unwrap_or(""),"effective_dataset_filter":"",
                "source_dataset_mismatch":0,"cache_type":"","validation_enabled":0,"success":1,"retrieval_quality":"","attachment_context":request["attachment_context"].as_str().unwrap_or("")});
            for (field,value) in [("resolved_dataset_ids",response["source_scope"]["resolved"].clone()),("resolved_dataset_names",json!([])),
                ("source_dataset_ids",response["source_scope"]["used"].clone()),("source_dataset_names",json!([])),("query_route_json",response["query_route"].clone()),
                ("retrieval_trace_json",response["retrieval_trace"].clone()),("artifact_json",response.get("artifact").cloned().unwrap_or(json!({}))) ] {row[field]=json!(value.to_string());}
            les_core::chat_request::journal::finish(&database,&id,history,&row,&response)?;Ok(())
        }).await.map_err(|_|"CHAT_STORAGE_UNAVAILABLE")??;
        self.completed = true;
        Ok(())
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        if !self.completed {
            let db = self.database.clone();
            let id = self.id.clone();
            let history = self.history;
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let _ = les_core::chat_request::journal::interrupt(&db, &id, history, None);
                });
            }
        }
    }
}
