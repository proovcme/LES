//! Trusted native tool dispatcher. Paths, scope and programs are bound by the server.
mod access;
mod local;
pub(super) mod sources;
use super::{prepare::Prepared, Context, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};
pub struct Tools {
    pub review: Option<Arc<super::document_review::ReviewTool>>,
    pub table: Option<Arc<super::table_document::TableTool>>,
    handle: u64,
    pub contracts: Vec<Value>,
    pub mcp: HashMap<String, (String, Value)>,
    access: String,
    sequence: std::sync::atomic::AtomicU64,
}
fn execute(request: Value) -> Result<Value> {
    let value = les_core::tool_execution::execute(&request);
    if value["ok"] == true {
        Ok(value["value"].clone())
    } else {
        Err(value["code"]
            .as_str()
            .unwrap_or("TOOL_EXECUTION_STATE_ERROR")
            .into())
    }
}
pub fn envelope(name: &str, operation: &str, result: Value, sources: Vec<Value>) -> Value {
    json!({"schema":"les_tool_result_v1","tool":name,"operation":operation,"inputs":[],"status":"ok",
        "result":result,"sources":sources,"missing":[],"warnings":[],"decision_required_from_model":false,"trace":"rust.chat.tools"})
}
impl Tools {
    pub async fn new(context: &Context, turn: &Prepared) -> Result<Self> {
        let database = context.database.clone();
        let profile = turn.profile.clone();
        let registered = turn.session["registered"] == true && turn.profile["mode"] == "agent";
        let grounded = turn.scope["document_grounding_enabled"] == true;
        let selected_only = turn.request["selected_sources_only"] == true;
        let remote = turn.egress_tag.is_some();
        tokio::task::spawn_blocking(move||->Result<Self>{
            let result=super::unwrap(les_core::tool_registry::execute(&json!({"schema":"les.tool-registry.v1","database":database,"action":"registry"})))?;
            let allowed=profile["tools"].as_array().cloned().unwrap_or_default();
            let mut contracts:Vec<_>=result["tools"].as_array().ok_or("TOOL_REGISTRY_INVALID")?.iter()
                .filter(|t|allowed.contains(&t["name"])&&(grounded||!["dataset","source"].iter().any(|c|t["category"]==*c))
                    && (!selected_only || (t["name"]!="web_search"&&t["name"]!="web_read"))
                    && (!remote || ["search_sources","read_source","read_pdf_source","read_excel_source"].iter().any(|n|t["name"]==*n))).cloned().collect();
            for contract in &mut contracts {
                if contract["name"]=="search_sources"{contract["timeout_seconds"]=json!(150);}
                if contract["name"]=="search_sources"||contract["name"]=="read_source"||contract["name"]=="read_excel_source"{
                    contract["result_budget"]=json!({"max_chars":24000,"max_items":32});
                }
            }
            if registered && !remote {
                let mut tool=super::unwrap(les_core::agent_files::execute(&json!({"schema":"les.agent-files.v1","action":"tool_contract","database":database,"state_root":"/","arguments":{}})))?;
                let object=tool.as_object_mut().ok_or("TOOL_REGISTRY_INVALID")?;
                object.extend(json!({"name":"agent_workspace","version":"1.0.0","title":"Память и рабочие файлы","category":"agent","result_schema":"les_tool_result_v1",
                    "effect":"local_state","scopes":["workspace"],"timeout_seconds":30,"retry":"safe","idempotency":"derived",
                    "result_budget":{"max_chars":16000,"max_items":32},"model_owned_fields":[],"provenance":"Bound workspace","tags":["agent"],"approval_required":false}).as_object().unwrap().clone());
                contracts.push(tool);
            }
            let mut mcp=HashMap::new();
            for connection in les_core::mcp_registry::list(&database).map_err(|_|"MCP_REGISTRY_UNAVAILABLE")?{
                for tool in connection["tools"].as_array().into_iter().flatten(){
                    if let (Some(id),Some(name))=(connection["id"].as_str(),tool["name"].as_str()){
                        let local=format!("mcp_{}",&les_core::sha256_hex(format!("{id}:{name}").as_bytes())[..32]);mcp.insert(local,(id.into(),tool.clone()));
                    }
                }
            }
            let access=access::contract(&contracts);let access_name=access["name"].as_str().ok_or("TOOL_REGISTRY_INVALID")?.to_owned();contracts.push(access);
            let handle=execute(json!({"schema":"les.tool-execution.v1","action":"new","database":database,"cursor_capacity":128,"cursor_ttl_seconds":300.}))?["handle"].as_u64().ok_or("TOOL_EXECUTION_STATE_ERROR")?;
            Ok(Self{handle,contracts,mcp,access:access_name,sequence:std::sync::atomic::AtomicU64::new(0),table:None,review:None})
        }).await.map_err(|_|"TOOL_REGISTRY_INVALID")?
    }
    pub fn bind_table(&mut self, table: Option<Arc<super::table_document::TableTool>>) {
        if table.is_some() {
            self.contracts.push(super::table_document::contract());
        }
        self.table = table;
        self.contracts.retain(|c| c["name"] != self.access);
        let access = access::contract(&self.contracts);
        self.access = access["name"].as_str().unwrap().to_owned();
        self.contracts.push(access);
    }
    pub fn bind_review(&mut self, review: Option<Arc<super::document_review::ReviewTool>>) {
        if review.is_some() {
            self.contracts.push(super::document_review::contract());
        }
        self.review = review;
        self.contracts.retain(|c| c["name"] != self.access);
        let access = access::contract(&self.contracts);
        self.access = access["name"].as_str().unwrap().to_owned();
        self.contracts.push(access);
    }
    #[cfg(test)]
    pub fn schemas(&self, limit: usize, grounded: bool) -> Vec<Value> {
        self.disclosed_schemas(limit, grounded, &[])
    }
    pub fn disclosed_schemas(
        &self,
        limit: usize,
        grounded: bool,
        disclosed: &[String],
    ) -> Vec<Value> {
        // The access tool keeps every granted read/compute capability reachable without
        // sending dozens of schemas into a small model's context on every round.
        let priority = if grounded {
            [
                "table_document",
                "search_sources",
                "read_pdf_source",
                "agent_workspace",
                "read_source",
            ]
        } else {
            [
                "table_document",
                "agent_workspace",
                "search_sources",
                "read_pdf_source",
                "read_source",
            ]
        };
        let mut chosen = vec![self
            .contracts
            .iter()
            .find(|c| c["name"] == self.access)
            .unwrap()];
        for name in disclosed.iter().rev() {
            if chosen.len() >= limit.max(1) {
                break;
            }
            if let Some(c) = self.contracts.iter().find(|c| c["name"] == *name) {
                if !chosen.iter().any(|v| v["name"] == c["name"]) {
                    chosen.push(c);
                }
            }
        }
        for name in priority {
            if chosen.len() >= limit.max(1) {
                break;
            }
            if let Some(c) = self.contracts.iter().find(|c| c["name"] == name) {
                if !chosen.iter().any(|v| v["name"] == c["name"]) {
                    chosen.push(c);
                }
            }
        }
        chosen.into_iter().map(|c|json!({"type":"function","function":{"name":c["name"],"description":c["summary"],"parameters":c["input_schema"]}})).collect()
    }
    pub fn disclosed(&self, payload: &Value) -> Option<String> {
        if payload["tool"] == self.access
            && payload["operation"] == "describe"
            && payload["status"] == "ok"
        {
            payload["result"]["name"]
                .as_str()
                .filter(|name| self.contracts.iter().any(|c| c["name"] == *name))
                .map(str::to_owned)
        } else {
            None
        }
    }
    pub fn run<'a>(
        &'a self,
        context: Arc<Context>,
        turn: &'a Prepared,
        call: &'a Value,
        run_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let name = call["tool"].as_str().ok_or("TOOL_NOT_REGISTERED")?;
            let contract = self
                .contracts
                .iter()
                .find(|c| c["name"] == name)
                .cloned()
                .ok_or("TOOL_NOT_ALLOWED")?;
            let args = call["args"].clone();
            let operation = args["operation"].as_str().map(str::to_owned);
            let scope = turn.scope["resolved_dataset_ids"].clone();
            let resolved = if let Some(id) = args["doc_id"].as_str().filter(|s| !s.is_empty()) {
                let database = context.database.clone();
                let id = id.to_owned();
                let document = tokio::task::spawn_blocking(move || {
                    super::unwrap(crate::library::catalog(
                        &database,
                        "library_document",
                        json!({"doc_id":id}),
                    ))
                })
                .await
                .map_err(|_| "TOOL_SCOPE_VIOLATION")??;
                if document.is_null() {
                    vec![]
                } else {
                    vec![document["dataset_id"].clone()]
                }
            } else {
                vec![]
            };
            let actor = turn.request["session_id"].as_str().unwrap_or("native-chat");
            // Provider IDs are only unique within a provider response. The local
            // receipt also binds the execution ordinal; repeated HTTP requests never
            // reach this point because the durable request receipt owns replay.
            let ordinal = self
                .sequence
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let call_id = format!(
                "{}:{}:{}",
                run_id,
                ordinal,
                &les_core::sha256_hex(call.to_string().as_bytes())[..24]
            );
            let request = json!({"call_id":call_id,"tool_name":name,"arguments":args,"allowed_dataset_ids":scope,"actor_id":actor,"actor_role":"user",
            "approval_receipt_id":null,"idempotency_key":call_id,"shadow":false,"allowed_attachment_ids":turn.request["attachment_id"].as_str().map(|id|vec![id]).unwrap_or_default()});
            let handle = self.handle;
            let admission = json!({"schema":"les.tool-execution.v1","action":"begin","handle":handle,"request":request,"contract":contract,
            "admission":{"remaining_seconds":600.,"resolved_dataset_ids":resolved,"local_state_allowed":(name=="agent_workspace"&&turn.session["registered"]==true)||(name=="table_document"&&self.table.is_some())||(name=="document_review"&&self.review.is_some())}});
            let admitted = tokio::task::spawn_blocking(move || execute(admission))
                .await
                .map_err(|_| "TOOL_EXECUTION_STATE_ERROR")??;
            if let Some(envelope) = admitted.get("envelope") {
                return Ok(self.payload(name, envelope, operation.as_deref()));
            }
            let timeout = admitted["timeout_seconds"]
                .as_f64()
                .unwrap_or(30.)
                .clamp(0.001, 1200.);
            let action = async {
                if name == "document_review" {
                    self.review
                        .as_ref()
                        .ok_or("TOOL_NOT_ALLOWED")?
                        .run(context, turn, args)
                        .await
                } else if name == "table_document" {
                    self.table
                        .as_ref()
                        .ok_or("TOOL_NOT_ALLOWED")?
                        .run(args)
                        .await
                } else if name == self.access {
                    access::run(self, context, turn, name, args, run_id).await
                } else if name.starts_with("filesystem_")
                    || [
                        "agent_workspace",
                        "list_installed_skills",
                        "read_installed_skill",
                        "run_skill_calculation",
                        "read_skill_calculation",
                    ]
                    .contains(&name)
                {
                    local::run(context, turn, name, args).await
                } else if name == "web_search" || name == "web_read" {
                    let mut input = args;
                    input["schema"] = json!("les.web.v1");
                    input["operation"] = json!(if name == "web_search" {
                        "search"
                    } else {
                        "read"
                    });
                    let value = crate::web::execute(&input).await;
                    let mut result = envelope(
                        name,
                        "public_web",
                        value.clone(),
                        value["sources"].as_array().cloned().unwrap_or_default(),
                    );
                    if value["status"] == "error" {
                        result["status"] = json!("error");
                        result["result"]["code"] = value["code"].clone();
                    }
                    Ok(result)
                } else if let Some((connection, expected)) = self.mcp.get(name) {
                    let value=crate::mcp::execute(&json!({"schema":"les.mcp.v1","database":context.database,"working_directory":context.state_root,
                    "action":"call","connection_id":connection,"expected":expected,"arguments":args})).await;
                    if value["ok"] != true {
                        return Err("MCP_REQUEST_FAILED".into());
                    }
                    let mut result = envelope(name, "mcp_call", value["value"].clone(), vec![]);
                    if value["value"]["isError"] == true {
                        result["status"] = json!("error");
                    }
                    Ok(result)
                } else {
                    sources::run(context, turn, name, args).await
                }
            };
            let value =
                tokio::time::timeout(std::time::Duration::from_secs_f64(timeout), action).await;
            let (raw, failure) = match value {
                Ok(Ok(raw)) => (raw, Value::Null),
                Ok(Err(error)) => {
                    let mut raw = envelope(
                        name,
                        operation.as_deref().unwrap_or("native_handler"),
                        error.payload(),
                        vec![],
                    );
                    raw["status"] = json!("error");
                    (raw, Value::Null)
                }
                Err(_) => (Value::Null, json!("timeout")),
            };
            let finish = json!({"schema":"les.tool-execution.v1","action":"finish","handle":handle,"ticket":admitted["ticket"],"raw":raw,"failure":failure});
            let mut finished = tokio::task::spawn_blocking(move || execute(finish))
                .await
                .map_err(|_| "TOOL_EXECUTION_STATE_ERROR")??;
            finished["envelope"]["duration_ms"] =
                json!(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
            eprintln!(
                "{}",
                json!({"event":"chat_tool_finished","tool":name,"status":finished["envelope"]["status"],"code":finished["envelope"]["code"],"duration_ms":finished["envelope"]["duration_ms"]})
            );
            Ok(self.payload(name, &finished["envelope"], operation.as_deref()))
        })
    }
    fn payload(&self, name: &str, execution: &Value, attempted: Option<&str>) -> Value {
        let mut metadata = execution.clone();
        if let Some(m) = metadata.as_object_mut() {
            m.remove("result");
        }
        if execution["result"]["schema"] == "les_tool_result_v1" {
            let mut result = execution["result"].clone();
            result["execution"] = metadata;
            return result;
        }
        let mut result = envelope(
            name,
            if ["document_review", "table_document", "agent_workspace"].contains(&name) {
                attempted.unwrap_or("trusted_execution")
            } else {
                "trusted_execution"
            },
            json!({"code":execution["code"],"reference":execution["cursor"]}),
            vec![],
        );
        result["status"] = json!("error");
        if execution["code"] == "TOOL_SCHEMA_INVALID" {
            if let Some(contract) = self.contracts.iter().find(|c| c["name"] == name) {
                let schema = &contract["input_schema"];
                if schema.to_string().len() <= 8192 {
                    result["result"]["input_schema"] = schema.clone();
                    result["result"]["instruction"]=json!("This tool is available, but this call was rejected before execution because its arguments do not match input_schema. Correct the arguments and call again. For extensions: list, describe the exact name, then call with operation, name and arguments. No action was performed by the rejected call.");
                }
            }
        }
        result["execution"] = metadata;
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_rejection_returns_the_granted_contract_without_argument_echo() {
        let schema = json!({"type":"object","properties":{"operation":{"type":"string"}},"required":["operation"],"additionalProperties":false});
        let tools = Tools {
            handle: 0,
            contracts: vec![json!({"name":"fixture","input_schema":schema})],
            mcp: HashMap::new(),
            access: "access".into(),
            sequence: std::sync::atomic::AtomicU64::new(0),
            table: None,
            review: None,
        };
        let result=tools.payload("fixture",&json!({"code":"TOOL_SCHEMA_INVALID","status":"rejected","result":{"arguments":"private-value"}}),None);
        assert_eq!(result["result"]["input_schema"], schema);
        assert_eq!(result["status"], "error");
        assert!(!result.to_string().contains("private-value"));
    }
    #[test]
    fn grounded_chat_advertises_search_and_pdf_without_revoking_workspace() {
        let contract =
            |name: &str| json!({"name":name,"summary":name,"input_schema":{"type":"object"}});
        let tools = Tools {
            handle: 0,
            contracts: vec![
                contract("access"),
                contract("agent_workspace"),
                contract("search_sources"),
                contract("read_pdf_source"),
            ],
            mcp: HashMap::new(),
            access: "access".into(),
            sequence: std::sync::atomic::AtomicU64::new(0),
            table: None,
            review: None,
        };
        let names = |grounded| {
            tools
                .schemas(3, grounded)
                .into_iter()
                .map(|t| t["function"]["name"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(true),
            vec![
                json!("access"),
                json!("search_sources"),
                json!("read_pdf_source")
            ]
        );
        assert_eq!(
            names(false),
            vec![
                json!("access"),
                json!("agent_workspace"),
                json!("search_sources")
            ]
        );
        assert!(tools
            .contracts
            .iter()
            .any(|c| c["name"] == "agent_workspace"));
    }
}
impl Drop for Tools {
    fn drop(&mut self) {
        let handle = self.handle;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || {
                let _ = execute(
                    json!({"schema":"les.tool-execution.v1","action":"close","handle":handle}),
                );
            });
        }
    }
}
