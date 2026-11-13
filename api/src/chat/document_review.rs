//! One native owner for model-owned review inventories, delivered spans and durable conclusions.
mod reader;
use super::{prepare::Prepared, unwrap, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
fn files(turn: &Prepared) -> Value {
    json!(crate::chat::tools::sources::selected_files(turn))
}
fn digest(value: &Value) -> Result<String> {
    Ok(les_core::sha256_hex(
        les_core::tool_execution::canonical_json(value)?.as_bytes(),
    ))
}
async fn raw(database: &Path, action: &str, args: Value) -> Result<Value> {
    let database = database.to_owned();
    let action = format!("review_{action}");
    tokio::task::spawn_blocking(move||les_core::document_tasks::execute(&json!({"schema":"les.document-tasks.v1","database":database,"action":action,"arguments":args}))).await.map_err(|_|"REVIEW_STORAGE_UNAVAILABLE".into())
}
async fn core(database: &Path, action: &str, args: Value) -> Result<Value> {
    unwrap(raw(database, action, args).await?)
}
pub struct ReviewTool {
    database: PathBuf,
    state: tokio::sync::Mutex<Value>,
    cache: tokio::sync::Mutex<reader::Cache>,
}
pub fn contract() -> Value {
    let mut c: Value =
        serde_json::from_str(include_str!("../../../config/document-review-tool.json"))
            .expect("checked review contract");
    c["effect"] = json!("local_state");
    c["timeout_seconds"] = json!(180);
    c
}
impl ReviewTool {
    pub async fn active(&self) -> bool {
        self.state.lock().await["task_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    }
    pub async fn bind(
        context: &Context,
        turn: &Prepared,
        available: bool,
    ) -> Result<Option<Arc<Self>>> {
        if !available
            || turn.scope["document_grounding_enabled"] != true
            || turn.scope["resolved_dataset_ids"]
                .as_array()
                .is_none_or(|v| v.is_empty())
            || turn.request["session_id"]
                .as_str()
                .is_none_or(|s| s.is_empty())
        {
            return Ok(None);
        }
        let mut datasets = turn.scope["resolved_dataset_ids"]
            .as_array()
            .unwrap()
            .clone();
        datasets.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        let mut files = files(turn).as_array().unwrap().clone();
        files.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        let owner = digest(
            &json!({"session":turn.request["session_id"],"project":turn.request["project_id"],"datasets":datasets,"scope":turn.request["scope"],"profile":turn.profile["revision_id"],"model":turn.connection.revision["revision_id"],"files":files}),
        )?;
        let database = context.state_root.join("data/document-tasks.sqlite3");
        core(&database, "init", json!({})).await?;
        let task = core(
            &database,
            "resolve_reference",
            json!({"owner":owner,"question":turn.request["question"]}),
        )
        .await?;
        let recent = core(&database, "recent_reviews", json!({"owner":owner})).await?;
        let state = json!({"owner":owner,"question":turn.request["question"],"task_id":task,"allowed":datasets,"files":files,"max_chars":(turn.connection.profile["input_token_limit"].as_u64().unwrap_or(24000)/8).clamp(200,4000),"current_read":{},"pending_read":null,"delivery_revision":0,"current_state":{"available_reviews":recent,"accepted":false}});
        Ok(Some(Arc::new(Self {
            database,
            state: tokio::sync::Mutex::new(state),
            cache: tokio::sync::Mutex::new(reader::Cache::default()),
        })))
    }
    async fn snapshots(
        &self,
        context: Arc<Context>,
        turn: &Prepared,
        selectors: &[Value],
    ) -> Result<(Value, Value, HashMap<String, Value>)> {
        let mut snapshots = json!({});
        let mut errors = json!({});
        let mut docs = HashMap::new();
        if selectors.is_empty() {
            return Ok((snapshots, errors, docs));
        }
        let mut originals = HashMap::new();
        let mut cache = self.cache.lock().await;
        let (config, _, _, _) =
            crate::chat::tools::sources::retrieval::configuration(context.clone(), false).await?;
        let store = context
            .index_store
            .as_ref()
            .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
        let collection = config["collection"]
            .as_str()
            .ok_or("INDEX_COLLECTION_MISMATCH")?;
        let stamp = store.revision(&context, collection).await?;
        for selector in selectors {
            let id = digest(selector)?;
            match reader::snapshot(
                context.clone(),
                turn,
                selector.clone(),
                &mut originals,
                &mut cache,
            )
            .await
            {
                Ok((snap, doc)) => {
                    snapshots[&id] = snap;
                    docs.insert(id, doc);
                }
                Err(e) => {
                    if e.code == "INDEX_CHANGED_DURING_SEARCH" {
                        return Err(e);
                    }
                    eprintln!(
                        "{}",
                        json!({"event":"review_source_check_failed","unit_id":id,"code":e.code})
                    );
                    errors[&id] = json!(e.code);
                }
            }
        }
        if store.revision(&context, collection).await? != stamp {
            return Err("INDEX_CHANGED_DURING_SEARCH".into());
        }
        Ok((snapshots, errors, docs))
    }
    async fn refresh(
        &self,
        context: Arc<Context>,
        turn: &Prepared,
        state: &mut Value,
    ) -> Result<Value> {
        if state["task_id"].as_str().is_none_or(|s| s.is_empty()) {
            return Ok(Value::Null);
        }
        let plan = core(
            &self.database,
            "plan",
            json!({"state":*state,"arguments":{"operation":"status"}}),
        )
        .await?;
        let (snapshots, errors, _) = self
            .snapshots(context, turn, plan["selectors"].as_array().unwrap())
            .await?;
        let result=core(&self.database,"refresh",json!({"state":*state,"task_id":state["task_id"],"snapshots":snapshots,"errors":errors})).await?;
        *state = result["state"].clone();
        Ok(result["payload"].clone())
    }
    pub async fn context(&self, context: Arc<Context>, turn: &Prepared) -> Result<Value> {
        let mut state = self.state.lock().await;
        self.refresh(context.clone(), turn, &mut state).await?;
        core(&self.database, "context", json!({"state":*state})).await
    }
    pub async fn acknowledge(&self, messages: Value) -> Result<()> {
        let mut state = self.state.lock().await;
        *state = core(
            &self.database,
            "ack",
            json!({"state":*state,"messages":messages}),
        )
        .await?;
        Ok(())
    }
    pub async fn revision(&self) -> Value {
        let state = self.state.lock().await;
        json!([
            state["task_id"],
            state["current_state"]["revision"],
            state["delivery_revision"]
        ])
    }
    pub async fn schema(&self) -> Value {
        let state = self.state.lock().await;
        let mut schema = contract()["input_schema"].clone();
        if state["task_id"].as_str().is_some_and(|v| !v.is_empty()) {
            // This turn already owns an inventory. Keep the executor's full
            // contract, but advertise progress instead of another idempotent start.
            schema["properties"]["operation"]["enum"] =
                json!(["read", "record", "status", "history"]);
        }
        schema
    }
    pub async fn assert_current(&self, context: Arc<Context>, turn: &Prepared) -> Result<()> {
        let state = self.state.lock().await;
        if state["current_read"]["source_fingerprint"].is_string() {
            let (snapshot, _) = reader::snapshot(
                context,
                turn,
                state["current_read"]["selector"].clone(),
                &mut HashMap::new(),
                &mut *self.cache.lock().await,
            )
            .await?;
            if snapshot["fingerprint"] != state["current_read"]["source_fingerprint"] {
                return Err("INDEX_CHANGED_DURING_SEARCH".into());
            }
        }
        Ok(())
    }
    pub async fn receipt(&self, context: Arc<Context>, turn: &Prepared) -> Result<Value> {
        let mut state = self.state.lock().await;
        let current = self.refresh(context, turn, &mut state).await?;
        if current.is_null() {
            return Ok(Value::Null);
        }
        Ok(les_core::execution_receipt(
            &json!({"action":"ledger_receipt","kind":"document_review","state":current}),
        )?["receipt"]
            .clone())
    }
    pub async fn run(&self, context: Arc<Context>, turn: &Prepared, args: Value) -> Result<Value> {
        let mut state = self.state.lock().await;
        let plan = raw(
            &self.database,
            "plan",
            json!({"state":*state,"arguments":args}),
        )
        .await?;
        if plan["status"] != "completed" {
            return Ok(failed(&args, plan));
        }
        let (snapshots, errors, docs) = self
            .snapshots(
                context,
                turn,
                plan["value"]["selectors"].as_array().unwrap(),
            )
            .await?;
        let result = raw(
            &self.database,
            "execute",
            json!({"state":*state,"arguments":args,"snapshots":snapshots,"errors":errors}),
        )
        .await?;
        if result["status"] != "completed" {
            return Ok(failed(&args, result));
        }
        *state = result["value"]["state"].clone();
        let payload = result["value"]["payload"].clone();
        let mut sources = Vec::new();
        if args["operation"] == "read" {
            let unit = payload["unit_id"]
                .as_str()
                .ok_or("SOURCE_IDENTITY_INVALID")?;
            let doc = docs.get(unit).ok_or("SOURCE_IDENTITY_INVALID")?;
            let mut source = crate::chat::tools::sources::reference(
                doc,
                payload["locator"]
                    .get("source_page")
                    .unwrap_or(&payload["locator"]["page"])
                    .clone(),
            );
            source["qdrant_point_id"] = payload["fragment_id"].clone();
            source["offset"] = payload["offset"].clone();
            sources.push(source);
        }
        Ok(super::tools::envelope(
            "document_review",
            args["operation"].as_str().unwrap_or("invalid"),
            payload,
            sources,
        ))
    }
}
fn failed(args: &Value, result: Value) -> Value {
    let mut e = super::tools::envelope(
        "document_review",
        args["operation"].as_str().unwrap_or("invalid"),
        json!({"code":result["code"],"detail":result["detail"]}),
        vec![],
    );
    e["status"] = json!("error");
    e
}
