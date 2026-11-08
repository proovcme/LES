//! Bound immutable attachment plus a durable native row ledger. No model paths.
use super::{prepare::Prepared, unwrap, Context, Result};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
pub struct TableTool {
    database: PathBuf,
    state: Mutex<Value>,
    pub document: Value,
}
fn call(database: &std::path::Path, action: &str, args: Value) -> Value {
    les_core::document_tasks::execute(
        &json!({"schema":"les.document-tasks.v1","database":database,"action":action,"arguments":args}),
    )
}
pub fn contract() -> Value {
    let mut contract = les_core::document_tasks::contract();
    contract["effect"] = json!("local_state");
    contract
}
impl TableTool {
    pub async fn bind(context: &Arc<Context>, turn: &Prepared) -> Result<Option<Arc<Self>>> {
        let mut datasets = turn.scope["resolved_dataset_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        datasets.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        let session = turn.request["session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or(les_core::chat_attachments::new_id()?);
        let scope = json!({"session":session,"project":turn.request["project_id"],"datasets":datasets,"scope":turn.request["scope"],"profile":turn.profile["revision_id"],"model":turn.connection.revision["revision_id"]});
        let key = les_core::sha256_hex(scope.to_string().as_bytes());
        let database = context.state_root.join("data/document-tasks.sqlite3");
        let saved = if let Some(id) = turn.request["attachment_id"].as_str() {
            let policy = crate::attachments::policy(context)
                .await
                .map_err(|e| super::error::Failure::new(&e))?;
            let ident = id.to_owned();
            let asset = tokio::task::spawn_blocking(move || {
                les_core::chat_attachments::resolve(&policy.root, &ident, policy.age)
            })
            .await
            .map_err(|_| "ATTACHMENT_READ_FAILED")??;
            if !asset.metadata["suffix"]
                .as_str()
                .is_some_and(|s| [".xlsx", ".xlsm", ".csv"].contains(&s))
            {
                return Ok(None);
            }
            let input = json!({"schema":"les.document-tasks.v1","database":database,"action":"open_source","arguments":{
                "scope_key":key,"question":turn.request["question"],"path":asset.path,"name":asset.metadata["original_name"],"source_sha256":asset.metadata["sha256"]}});
            drop(asset);
            let _slot = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                context.attachment_slots.clone().acquire_owned(),
            )
            .await
            .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?
            .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?;
            let value = crate::attachments::worker(json!({"action":"table_task","request":input}))
                .await
                .map_err(|e| super::error::Failure::new(&e))?;
            unwrap(value)?
        } else {
            if turn.request["session_id"]
                .as_str()
                .is_none_or(|s| s.is_empty())
                || !database.exists()
            {
                return Ok(None);
            }
            let db = database.clone();
            let result =
                tokio::task::spawn_blocking(move || call(&db, "latest", json!({"scope_key":key})))
                    .await
                    .map_err(|_| "TABLE_TASK_UNAVAILABLE")?;
            let mut saved = unwrap(result)?;
            if saved.is_null() {
                return Ok(None);
            }
            saved["document"] =
                serde_json::from_str(saved["source"].as_str().ok_or("TABLE_TASK_INVALID")?)
                    .map_err(|_| "TABLE_TASK_INVALID")?;
            saved
        };
        let state = json!({"owner":saved["owner"],"task_id":saved["id"],"question":turn.request["question"],"current":{},"work_requested":false,
            "packet_bytes":turn.connection.profile["input_token_limit"].as_u64().unwrap_or(24000).saturating_div(4).clamp(512,6000)});
        Ok(Some(Arc::new(Self {
            database,
            state: Mutex::new(state),
            document: saved["document"].clone(),
        })))
    }
    pub fn manifest(&self) -> Value {
        let d = &self.document;
        json!({"name":d["name"],"source_sha256":d["sha256"],"sheets":d["sheets"],"total_nonempty_rows":d["row_count"],"read":"table_document","cell_policy":d["cell_policy"]})
    }
    pub async fn state(self: &Arc<Self>) -> Result<Value> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let state = this.state.lock().map_err(|_| "TABLE_TASK_UNAVAILABLE")?;
            unwrap(call(
                &this.database,
                "bound_context",
                json!({"state":*state}),
            ))
        })
        .await
        .map_err(|_| "TABLE_TASK_UNAVAILABLE")?
    }
    pub async fn receipt(self: &Arc<Self>) -> Result<Value> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let state = this.state.lock().map_err(|_| "TABLE_TASK_UNAVAILABLE")?;
            if state["work_requested"] != true {
                return Ok(Value::Null);
            }
            Ok(unwrap(call(
                &this.database,
                "bound_receipt",
                json!({"state":*state}),
            ))?["receipt"]
                .clone())
        })
        .await
        .map_err(|_| "TABLE_TASK_UNAVAILABLE")?
    }
    pub async fn run(self: &Arc<Self>, args: Value) -> Result<Value> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = this.state.lock().map_err(|_| "TABLE_TASK_UNAVAILABLE")?;
            if args["operation"]
                .as_str()
                .is_some_and(|s| ["start", "next", "record"].contains(&s))
            {
                state["work_requested"] = json!(true);
            }
            let result = call(
                &this.database,
                "bound_execute",
                json!({"state":*state,"arguments":args}),
            );
            let mut payload = super::tools::envelope(
                "table_document",
                args["operation"].as_str().unwrap_or("invalid"),
                result["value"]["payload"].clone(),
                vec![],
            );
            if result["status"] == "completed" {
                *state = result["value"]["state"].clone();
            } else {
                payload["status"] = json!("error");
                payload["result"] = json!({"code":result["code"],"detail":result["detail"]});
            }
            Ok(payload)
        })
        .await
        .map_err(|_| "TABLE_TASK_UNAVAILABLE")?
    }
}
