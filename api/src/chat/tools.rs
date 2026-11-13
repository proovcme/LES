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
    indexed_reader_first: bool,
    pub(super) read_max_chars: usize,
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
/// Model-facing readers use one registered identity. Public API compatibility
/// and validation of explicit dataset/path arguments remain in the full contract.
fn model_schema(contract: &Value) -> Value {
    let mut schema = contract["input_schema"].clone();
    if !["read_source", "read_pdf_source", "read_excel_source"]
        .iter()
        .any(|name| contract["name"] == *name)
        || !schema["properties"]["doc_id"].is_object()
    {
        return schema;
    }
    if let Some(properties) = schema["properties"].as_object_mut() {
        // The tool summary already explains frozen continuations. Repeating
        // prose in every hash/revision property costs context on every turn;
        // retain all validation constraints and the full API/MCP contract.
        for property in properties.values_mut() {
            if let Some(value) = property.as_object_mut() {
                value.remove("description");
            }
        }
        properties.remove("dataset_id");
        properties.remove("doc_name");
        if contract["name"] == "read_source" {
            // The ordinary text reader must not advertise Word media as
            // document sections. The full media contract remains available
            // via extension describe/call and the public API/MCP.
            properties.remove("member");
            properties.remove("part_id");
            if let Some(view) = properties.get_mut("view") {
                view["enum"] = json!(["text"]);
            }
        }
    }
    let mut required = schema["required"].as_array().cloned().unwrap_or_default();
    required.retain(|key| key != "dataset_id" && key != "doc_name");
    if !required.contains(&json!("doc_id")) {
        required.push(json!("doc_id"));
    }
    schema["required"] = json!(required);
    schema.as_object_mut().unwrap().remove("anyOf");
    schema
}
impl Tools {
    pub async fn new(context: &Context, turn: &Prepared) -> Result<Self> {
        let database = context.database.clone();
        let profile = turn.profile.clone();
        let registered = turn.session["registered"] == true && turn.profile["mode"] == "agent";
        let grounded = turn.scope["document_grounding_enabled"] == true;
        let push = turn.request["retrieval_mode"] == "push";
        let selected_only = turn.request["selected_sources_only"] == true;
        let indexed_reader_first = prefer_indexed_reader(&sources::selected_files(turn));
        let remote = turn.egress_tag.is_some();
        tokio::task::spawn_blocking(move||->Result<Self>{
            let result=super::unwrap(les_core::tool_registry::execute(&json!({"schema":"les.tool-registry.v1","database":database,"action":"registry"})))?;
            let allowed=profile["tools"].as_array().cloned().unwrap_or_default();
            let mut contracts:Vec<_>=result["tools"].as_array().ok_or("TOOL_REGISTRY_INVALID")?.iter()
                .filter(|t|allowed.contains(&t["name"])&&(grounded||!["dataset","source"].iter().any(|c|t["category"]==*c))
                    && (!push || !["dataset","source"].iter().any(|c|t["category"]==*c))
                    && (!selected_only || (t["name"]!="web_search"&&t["name"]!="web_read"))
                    && (!remote || ["search_sources","read_source","read_pdf_source","read_excel_source"].iter().any(|n|t["name"]==*n))).cloned().collect();
            for contract in &mut contracts {
                if contract["name"]=="search_sources"{contract["timeout_seconds"]=json!(150);}
                if [
                    "search_sources",
                    "read_source",
                    "read_excel_source",
                    "dataset_map",
                ]
                .iter()
                .any(|n| contract["name"] == *n)
                {
                    // The executor counts every nested array item, including
                    // repeated citation identities, receipt IDs and discovery
                    // cards. This is not the number of factual passages (32,
                    // enforced by the reader). Keep the independent char cap.
                    contract["result_budget"]=json!({"max_chars":24000,"max_items":256});
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
            Ok(Self{handle,contracts,mcp,access:access_name,indexed_reader_first,read_max_chars:16000,sequence:std::sync::atomic::AtomicU64::new(0),table:None,review:None})
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
        self.disclosed_schemas(self.schema_limit(limit, 5, grounded), grounded, &[])
    }
    pub fn schema_limit(&self, normal: usize, maximum: usize, grounded: bool) -> usize {
        // These are distinct evidence operations. A mixed corpus must not hide
        // the physical reader behind extension discovery just because a Word
        // result was also found. The caller budgets the actual schema size.
        let limit = if grounded
            && ["read_source", "read_pdf_source"]
                .iter()
                .all(|name| self.contracts.iter().any(|c| c["name"] == *name))
        {
            normal.max(4)
        } else {
            normal.max(1)
        };
        limit.min(maximum.max(1))
    }
    pub fn disclosed_schemas(
        &self,
        limit: usize,
        grounded: bool,
        disclosed: &[String],
    ) -> Vec<Value> {
        // The access tool keeps every granted read/compute capability reachable without
        // sending dozens of schemas into a small model's context on every round.
        let (reader, other_reader) = if self.indexed_reader_first {
            ("read_source", "read_pdf_source")
        } else {
            ("read_pdf_source", "read_source")
        };
        let priority = if grounded {
            [
                "table_document",
                "search_sources",
                reader,
                other_reader,
                "agent_workspace",
            ]
        } else {
            [
                "table_document",
                "agent_workspace",
                "search_sources",
                reader,
                other_reader,
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
        chosen.into_iter().map(|c| {
            let schema=model_schema(c);
            let description=if c["name"] == "read_source" {
                "Read structured Word originals or indexed text by doc_id. q/clause locates text; continue with offset and the returned index_revision/view_sha256. Physical PDF pages use read_pdf_source. Word images/attachments use the extension's read_source view=parts/part.".to_owned()
            } else if c["name"] == "read_pdf_source" {
                "Read an original PDF page by doc_id and 1-based page; configured OCR handles scans. source_sha256 pins the original. Continue with offset and returned view_sha256. Verify OCR numbers against the original.".to_owned()
            } else if schema!=c["input_schema"] {
                format!("{} Use only the registered doc_id from source references or document search to identify the document. Dataset and filename are resolved by LES.",
                    c["summary"].as_str().unwrap_or("").replace("Use exact dataset_id+doc_name from sources; never invent ids or paths.", ""))
            } else { c["summary"].as_str().unwrap_or("").to_owned() };
            json!({"type":"function","function":{"name":c["name"],"description":description,"parameters":schema}})
        }).collect()
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
    pub fn observe_sources(&mut self, payload: &Value) {
        let payload = super::protocol::effective(payload);
        if payload["status"] != "ok" {
            return;
        }
        let names: Vec<_> = payload["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["doc_name"].as_str())
            .chain(
                payload["result"]["documents"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s["file_name"].as_str()),
            )
            .chain(
                payload["result"]["candidate_documents"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s["file_name"].as_str()),
            )
            .map(|name| json!(name))
            .collect();
        if !names.is_empty() {
            self.indexed_reader_first = prefer_indexed_reader(&names);
        }
    }
    pub fn run<'a>(
        &'a self,
        context: Arc<Context>,
        turn: &'a Prepared,
        call: &'a Value,
        run_id: &'a str,
        deadline: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            super::budget::remaining(deadline)?;
            let started = std::time::Instant::now();
            let name = call["tool"].as_str().ok_or("TOOL_NOT_REGISTERED")?;
            let contract = self
                .contracts
                .iter()
                .find(|c| c["name"] == name)
                .cloned()
                .ok_or("TOOL_NOT_ALLOWED")?;
            let mut args = call["args"].clone();
            let read_grant = bounded_read(name, &mut args, self.read_max_chars, &contract);
            let source_request = source_request(name, &args);
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
            "admission":{"remaining_seconds":super::budget::remaining(deadline)?.as_secs_f64(),"resolved_dataset_ids":resolved,"local_state_allowed":(name=="agent_workspace"&&turn.session["registered"]==true)||(name=="table_document"&&self.table.is_some())||(name=="document_review"&&self.review.is_some())}});
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
                    access::run(self, context, turn, name, args, run_id, deadline).await
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
                    let mut args = args;
                    if ["search_sources", "read_source", "read_excel_source"].contains(&name) {
                        args["_context_budget_chars"] = json!(self.read_max_chars.min(16000));
                    }
                    sources::run(context, turn, name, args).await
                }
            };
            // The extension wrapper must let its nested execution finish its receipt.
            // The target receives this same deadline and owns cancellation of its IO.
            let value = if name == self.access {
                match super::budget::remaining(deadline) {
                    Ok(_) => action.await,
                    Err(error) => Err(error),
                }
            } else {
                let end = deadline.min(started + std::time::Duration::from_secs_f64(timeout));
                super::budget::until(end, action).await
            };
            let (raw, failure) = match value {
                Ok(raw) => (raw, Value::Null),
                Err(error) if error.code == "CHAT_DEADLINE_EXCEEDED" => {
                    (Value::Null, json!("timeout"))
                }
                Err(error) => {
                    let mut raw = envelope(
                        name,
                        operation.as_deref().unwrap_or("native_handler"),
                        error.payload(),
                        vec![],
                    );
                    raw["status"] = json!("error");
                    (raw, Value::Null)
                }
            };
            let mut raw = raw;
            if let Some(grant) = read_grant {
                if raw["result"].is_object() {
                    raw["result"]["context_budget"] = grant;
                }
            }
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
            let mut payload = self.payload(name, &finished["envelope"], operation.as_deref());
            if let Some(request) = source_request {
                payload["inputs"] = json!([request]);
            }
            Ok(payload)
        })
    }
    fn payload(&self, name: &str, execution: &Value, attempted: Option<&str>) -> Value {
        let mut metadata = execution.clone();
        if let Some(m) = metadata.as_object_mut() {
            m.remove("result");
        }
        if execution["result"]["schema"] == "les_tool_result_v1"
            && execution["result"]["tool"] == name
        {
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
            json!({"code":if execution["code"]=="TOOL_OK" {json!("TOOL_RESULT_INVALID")} else {execution["code"].clone()},"reference":execution["cursor"]}),
            vec![],
        );
        result["status"] = json!("error");
        if execution["code"] == "TOOL_RESULT_BUDGET_EXCEEDED" {
            result["result"]["instruction"] = json!("The full result is stored, but does not fit this tool response. It is not admitted evidence. Repeat a narrower query or reduce limit/max_chars; do not infer the missing contents.");
        }
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
/// An explicitly selected non-PDF needs the generic reader in the small initial
/// tool window. This only orders granted schemas; it never rewrites a tool call
/// or treats a failed physical read as successful.
fn prefer_indexed_reader(selected: &[Value]) -> bool {
    !selected.is_empty()
        && selected.iter().any(|name| {
            name.as_str()
                .is_none_or(|name| !name.to_ascii_lowercase().ends_with(".pdf"))
        })
}
fn source_request(name: &str, args: &Value) -> Option<Value> {
    if ![
        "search_sources",
        "read_source",
        "read_pdf_source",
        "read_excel_source",
    ]
    .contains(&name)
    {
        return None;
    }
    let mut values = serde_json::Map::new();
    for key in [
        "doc_id",
        "dataset_id",
        "dataset_ids",
        "doc_name",
        "view",
        "q",
        "clause",
        "page",
        "offset",
        "limit",
        "max_chars",
    ] {
        if let Some(value) = args.get(key) {
            let value = if let Some(text) = value.as_str().filter(|s| s.chars().count() > 2048) {
                json!({"prefix":text.chars().take(2048).collect::<String>(),"sha256":les_core::sha256_hex(text.as_bytes()),"truncated":true})
            } else {
                value.clone()
            };
            values.insert(key.into(), value);
        }
    }
    Some(json!({"kind":"source_request","arguments":values}))
}
fn bounded_read(name: &str, args: &mut Value, cap: usize, contract: &Value) -> Option<Value> {
    if !["read_source", "read_pdf_source", "read_excel_source"].contains(&name) || !args.is_object()
    {
        return None;
    }
    if args.get("max_chars").is_some_and(|v| v.as_u64().is_none()) {
        return None;
    }
    // The host's default must obey the reader's own contract as well as the
    // model window. Otherwise a valid page request fails schema admission
    // before the physical reader can report its actual reason.
    let cap = cap.min(
        contract["input_schema"]["properties"]["max_chars"]["maximum"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(cap),
    );
    let explicit = args["max_chars"].as_u64();
    let requested = explicit.unwrap_or(cap.min(16000) as u64);
    // reader_capacity already converts the token window to characters and
    // reserves the call envelope. Halving this character budget again made a
    // fitting materialized page unreadable on its next cursor position.
    let granted = requested.min(cap.max(1) as u64);
    if explicit.is_some() && requested <= granted {
        return None;
    }
    args["max_chars"] = json!(granted);
    Some(
        json!({"requested_max_chars":requested,"granted_max_chars":granted,"reason":"reader_contract_and_model_context_budget",
        "note":"Размер порции ограничен свободным окном модели с резервом для идентификаторов и квитанции. Это не полный просмотр. Продолжай с точным next_offset при неизменном запросе, документе и редакции."}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reader_schema_has_one_identity_but_keeps_page_and_revision_constraints() {
        let contract = json!({"name":"read_pdf_source","input_schema":{"type":"object","properties":{
            "doc_id":{"type":"string"},"dataset_id":{"type":"string"},"doc_name":{"type":"string"},
            "page":{"type":"integer","minimum":1},"source_sha256":{"type":"string"}},
            "required":["page"],"anyOf":[{"required":["doc_id"]},{"required":["dataset_id","doc_name"]}],"additionalProperties":false}});
        let original = contract.clone();
        let schema = model_schema(&contract);
        assert_eq!(schema["required"], json!(["page", "doc_id"]));
        assert_eq!(schema["properties"]["page"]["minimum"], 1);
        assert!(schema["properties"]["source_sha256"].is_object());
        assert!(schema["properties"].get("dataset_id").is_none());
        assert!(schema["properties"].get("doc_name").is_none());
        assert!(schema.get("anyOf").is_none());
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(contract, original);
    }
    #[test]
    fn ordinary_text_schema_does_not_mislabel_media_as_sections() {
        let full = json!({"name":"read_source","input_schema":{"type":"object","properties":{
            "doc_id":{"type":"string"},"view":{"type":"string","enum":["text","parts","part"]},
            "member":{"type":"string"},"part_id":{"type":"string"},"offset":{"type":"integer","minimum":0}},
            "required":[],"additionalProperties":false}});
        let schema = model_schema(&full);
        assert_eq!(schema["properties"]["view"]["enum"], json!(["text"]));
        assert!(schema["properties"].get("member").is_none());
        assert!(schema["properties"].get("part_id").is_none());
        assert_eq!(schema["properties"]["offset"]["minimum"], 0);
        assert_eq!(
            full["input_schema"]["properties"]["view"]["enum"],
            json!(["text", "parts", "part"])
        );
    }
    #[test]
    fn an_overflow_reference_keeps_tool_identity_and_is_never_evidence() {
        let tools = Tools {
            handle: 0,
            contracts: vec![],
            mcp: HashMap::new(),
            access: "access".into(),
            indexed_reader_first: false,
            read_max_chars: 16000,
            sequence: std::sync::atomic::AtomicU64::new(0),
            table: None,
            review: None,
        };
        let receipt = json!({"status":"overflow","code":"TOOL_RESULT_BUDGET_EXCEEDED","cursor":"stored-result",
            "result":{"schema":"les_tool_result_v1","reference":"stored-result","whole_result_stored":true}});
        let payload = tools.payload("search_sources", &receipt, None);
        assert_eq!(payload["tool"], "search_sources");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["result"]["code"], "TOOL_RESULT_BUDGET_EXCEEDED");
        assert_eq!(payload["result"]["reference"], "stored-result");
        assert!(payload["sources"].as_array().unwrap().is_empty());
        let mut agent =
            les_core::coordinator::Coordinator::new(2, 30., ["search_sources".into()]).unwrap();
        assert!(matches!(
            agent.next_blocking().unwrap(),
            les_core::coordinator::Step::Model { .. }
        ));
        agent.model_result(json!({"tool_calls":[{"id":"one","type":"function","function":{"name":"search_sources","arguments":"{}"}}]})).unwrap();
        assert!(matches!(
            agent.next_blocking().unwrap(),
            les_core::coordinator::Step::Tool(_)
        ));
        agent.tool_result(payload).unwrap();
        assert!(matches!(
            agent.next_blocking().unwrap(),
            les_core::coordinator::Step::Failed(code) if code=="TOOL_RESULT_BUDGET_EXCEEDED"
        ));
    }
    #[test]
    fn mixed_discovery_uses_all_formats_independently_of_the_first_ranked_source() {
        let mut tools = Tools {
            handle: 0,
            contracts: ["access", "search_sources", "read_source", "read_pdf_source"]
                .into_iter()
                .map(|name| json!({"name":name,"summary":name,"input_schema":{"type":"object"}}))
                .collect(),
            mcp: HashMap::new(),
            access: "access".into(),
            indexed_reader_first: false,
            read_max_chars: 16000,
            sequence: std::sync::atomic::AtomicU64::new(0),
            table: None,
            review: None,
        };
        for names in [["Source.pdf", "Book.xlsx"], ["Book.xlsx", "Source.pdf"]] {
            tools.observe_sources(
                &json!({"status":"ok","sources":names.map(|name|json!({"doc_name":name}))}),
            );
            assert!(tools
                .schemas(3, true)
                .iter()
                .any(|s| s["function"]["name"] == "read_source"));
        }
        tools.observe_sources(&json!({"status":"ok","result":{"documents":[{"file_name":"Source.pdf"},{"file_name":"Book.xlsx"}]}}));
        assert!(tools
            .schemas(3, true)
            .iter()
            .any(|s| s["function"]["name"] == "read_source"));
        tools.observe_sources(&json!({"status":"ok","sources":[{"doc_name":"Only.pdf"}]}));
        assert!(tools
            .schemas(3, true)
            .iter()
            .any(|s| s["function"]["name"] == "read_pdf_source"));
        assert_eq!(tools.schema_limit(3, 5, true), 4);
        assert_eq!(tools.schema_limit(2, 2, true), 2);
        assert_eq!(tools.schema_limit(3, 5, false), 3);
    }
    #[test]
    fn schema_rejection_returns_the_granted_contract_without_argument_echo() {
        let schema = json!({"type":"object","properties":{"operation":{"type":"string"}},"required":["operation"],"additionalProperties":false});
        let tools = Tools {
            handle: 0,
            contracts: vec![json!({"name":"fixture","input_schema":schema})],
            mcp: HashMap::new(),
            access: "access".into(),
            indexed_reader_first: false,
            read_max_chars: 16000,
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
            indexed_reader_first: false,
            read_max_chars: 16000,
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
    #[test]
    fn selected_word_and_mixed_files_expose_generic_reader_without_revoking_pdf() {
        let contract =
            |name: &str| json!({"name":name,"summary":name,"input_schema":{"type":"object"}});
        for selected in [
            vec![json!("folder/Source.DOCX")],
            vec![json!("a.pdf"), json!("b.xlsx")],
        ] {
            let tools = Tools {
                handle: 0,
                contracts: vec![
                    contract("access"),
                    contract("search_sources"),
                    contract("read_pdf_source"),
                    contract("read_source"),
                ],
                mcp: HashMap::new(),
                access: "access".into(),
                indexed_reader_first: prefer_indexed_reader(&selected),
                read_max_chars: 16000,
                sequence: std::sync::atomic::AtomicU64::new(0),
                table: None,
                review: None,
            };
            let names: Vec<_> = tools
                .schemas(3, true)
                .iter()
                .map(|s| s["function"]["name"].clone())
                .collect();
            assert_eq!(
                names,
                vec![
                    json!("access"),
                    json!("search_sources"),
                    json!("read_source"),
                    json!("read_pdf_source")
                ]
            );
            let disclosed = tools.disclosed_schemas(3, true, &["read_pdf_source".into()]);
            assert!(disclosed
                .iter()
                .any(|s| s["function"]["name"] == "read_pdf_source"));
        }
        assert!(!prefer_indexed_reader(&[]));
        assert!(!prefer_indexed_reader(&[json!("folder/Source.PDF")]));
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
