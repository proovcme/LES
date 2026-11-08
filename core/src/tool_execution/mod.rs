//! Trusted executor state and policy. Dispatchers provide effects; they cannot bypass admission/results policy.
pub(crate) mod canonical;
pub use canonical::encode as canonical_json;
mod store;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, &'static str>;
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs_f64())
        .unwrap_or(0.)
}
fn identifier() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "TOOL_EXECUTION_STATE_ERROR")?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}
fn approval_effect(contract: &Value) -> bool {
    matches!(
        contract["effect"].as_str(),
        Some("commit" | "external" | "destructive")
    )
}
fn shadow_effect(contract: &Value) -> bool {
    matches!(
        contract["effect"].as_str(),
        Some("local_state" | "draft" | "commit" | "external" | "destructive")
    ) || contract["tags"]
        .as_array()
        .is_some_and(|v| v.contains(&json!("shadow_validate_only")))
}
fn key(request: &Value) -> &str {
    crate::auth::trim(request["idempotency_key"].as_str().unwrap_or(""))
}
fn envelope(request: &Value, status: &str, code: &str, result: Value) -> Value {
    json!({"schema":"les_tool_execution_v1","call_id":request["call_id"],"tool_name":request["tool_name"],"status":status,"code":code,"result":result,"cursor":null,"omitted_items":0})
}
fn stopped(request: &Value, status: &str, code: &str) -> Value {
    json!({"envelope":envelope(request,status,code,json!({}))})
}
struct Pending {
    request: Value,
    contract: Value,
    fingerprint: String,
    claimed: bool,
    deadline: Instant,
}
struct Cursor {
    key: String,
    actor: String,
    expires: Instant,
    raw: Value,
}
struct Executor {
    closed: bool,
    database: Option<String>,
    ttl: f64,
    capacity: usize,
    idempotency: HashMap<(String, String), (String, Value)>,
    cursors: VecDeque<Cursor>,
    pending: HashMap<String, Pending>,
}
impl Executor {
    fn store(&self, action: &str, args: &Value) -> Result<Value> {
        store::execute(
            self.database
                .as_deref()
                .ok_or("TOOL_DURABLE_IDEMPOTENCY_REQUIRED")?,
            action,
            args,
        )
    }
    fn cursor(&mut self, key: &str, actor: &str) -> Result<Value> {
        if self.database.is_some() {
            let value = self.store("resolve_cursor", &json!({"cursor":key,"actor_id":actor}))?;
            if !value.is_null() {
                return Ok(value);
            }
        }
        if let Some(index) = self
            .cursors
            .iter()
            .position(|v| v.key == key && v.actor == actor && v.expires > Instant::now())
        {
            let value = self.cursors.remove(index).unwrap();
            let raw = value.raw.clone();
            self.cursors.push_back(value);
            Ok(raw)
        } else {
            Ok(Value::Null)
        }
    }
    fn begin(&mut self, request: Value, contract: Value, admission: &Value) -> Result<Value> {
        let started = Instant::now();
        if !contract.is_object() {
            return Ok(stopped(&request, "rejected", "TOOL_NOT_REGISTERED"));
        }
        if ["call_id", "actor_id"].iter().any(|key| {
            !request[*key]
                .as_str()
                .is_some_and(|s| !crate::auth::trim(s).is_empty())
        }) {
            return Ok(stopped(&request, "rejected", "TOOL_REQUEST_INVALID"));
        }
        if !request["arguments"].is_object() {
            return Ok(stopped(&request, "rejected", "TOOL_SCHEMA_INVALID"));
        }
        let schema = &contract["input_schema"];
        if schema["type"] != "object" {
            return Ok(stopped(&request, "rejected", "TOOL_SCHEMA_UNSUPPORTED"));
        }
        if crate::tool_registry::local_schema(schema).is_err() {
            return Ok(stopped(&request, "rejected", "TOOL_SCHEMA_INVALID"));
        }
        if !jsonschema::validator_for(schema)
            .map_err(|_| "TOOL_SCHEMA_INVALID")?
            .is_valid(&request["arguments"])
        {
            return Ok(stopped(&request, "rejected", "TOOL_SCHEMA_INVALID"));
        }
        let remaining = admission["remaining_seconds"]
            .as_f64()
            .filter(|v| v.is_finite())
            .unwrap_or(0.);
        if remaining <= 0. {
            return Ok(stopped(&request, "timeout", "TOOL_DEADLINE_EXCEEDED"));
        }
        if scope_escapes(&contract, &request, &admission["resolved_dataset_ids"]) {
            return Ok(stopped(&request, "rejected", "TOOL_SCOPE_VIOLATION"));
        }
        if request["shadow"] == true && shadow_effect(&contract) {
            return Ok(
                json!({"envelope":envelope(&request,"shadow","TOOL_WOULD_EXECUTE",json!({"effect":contract["effect"],"persisted":false}))}),
            );
        }
        let hash = crate::sha256_hex(canonical::encode(&request["arguments"])?.as_bytes());
        let fingerprint = format!("{}:{hash}", request["tool_name"].as_str().unwrap_or(""));
        if approval_effect(&contract) && request["actor_role"] != "admin" {
            return Ok(stopped(&request, "rejected", "TOOL_AUTHORIZATION_REQUIRED"));
        }
        if contract["effect"] == "local_state" && admission["local_state_allowed"] != true {
            return Ok(stopped(
                &request,
                "rejected",
                "TOOL_LOCAL_STATE_PERMISSION_REQUIRED",
            ));
        }
        if approval_effect(&contract) || contract["approval_required"] == true {
            let matches = if self.database.is_some() {
                let approved = self.store(
                    "approval",
                    &json!({"receipt_id":request["approval_receipt_id"].as_str().unwrap_or("")}),
                )?;
                !approved.is_null()
                    && approved["status"] == "approved"
                    && approved["expires_at_epoch"]
                        .as_f64()
                        .is_some_and(|v| v > now())
                    && request["arguments"]["proposal_revision"]
                        .as_str()
                        .is_some_and(|v| !v.is_empty())
                    && approved["proposal_revision"] == request["arguments"]["proposal_revision"]
                    && approved["tool_name"] == contract["name"]
                    && approved["actor_id"] == request["actor_id"]
                    && approved["argument_sha256"] == hash
            } else {
                false
            };
            if !matches {
                return Ok(stopped(&request, "rejected", "TOOL_APPROVAL_REQUIRED"));
            }
        }
        let mut claimed = false;
        let idem = key(&request);
        if (contract["idempotency"] == "required" || approval_effect(&contract)) && idem.is_empty()
        {
            return Ok(stopped(
                &request,
                "rejected",
                "TOOL_IDEMPOTENCY_KEY_REQUIRED",
            ));
        }
        if !idem.is_empty() {
            if approval_effect(&contract) {
                if self.database.is_none() {
                    return Ok(stopped(
                        &request,
                        "rejected",
                        "TOOL_DURABLE_IDEMPOTENCY_REQUIRED",
                    ));
                }
                let state=self.store("claim",&json!({"actor_id":request["actor_id"],"tool_name":request["tool_name"],"idempotency_key":idem,"fingerprint":fingerprint,"approval_receipt_id":request["approval_receipt_id"].as_str().unwrap_or(""),"verify_approval":true,"proposal_revision":request["arguments"]["proposal_revision"],"argument_sha256":hash}))?;
                match state["state"].as_str() {
                    Some("claimed") => claimed = true,
                    Some("completed") => {
                        let value = &state["envelope"];
                        if value["schema"] != "les_tool_execution_v1"
                            || !value["result"].is_object()
                            || ["call_id", "tool_name", "status", "code"]
                                .iter()
                                .any(|key| !value[*key].is_string())
                        {
                            return Err("TOOL_EXECUTION_STATE_ERROR");
                        }
                        if let Some(cursor) = value["cursor"].as_str() {
                            if self
                                .cursor(cursor, request["actor_id"].as_str().unwrap_or(""))?
                                .is_null()
                            {
                                return Ok(stopped(
                                    &request,
                                    "rejected",
                                    "TOOL_RESULT_REFERENCE_EXPIRED",
                                ));
                            }
                        }
                        return Ok(json!({"envelope":value}));
                    }
                    Some(state) => {
                        return Ok(stopped(
                            &request,
                            "rejected",
                            match state {
                                "conflict" => "TOOL_IDEMPOTENCY_CONFLICT",
                                "in_progress" => "TOOL_EXECUTION_IN_PROGRESS",
                                "ambiguous" => "TOOL_EXECUTION_AMBIGUOUS",
                                "approval_used" => "TOOL_APPROVAL_ALREADY_USED",
                                "approval_invalid" => "TOOL_APPROVAL_REQUIRED",
                                _ => "TOOL_IDEMPOTENCY_STATE_INVALID",
                            },
                        ))
                    }
                    _ => return Err("TOOL_EXECUTION_STATE_ERROR"),
                }
            } else if let Some((previous, value)) = self.idempotency.get(&(
                request["actor_id"].as_str().unwrap_or("").into(),
                idem.into(),
            )) {
                if previous != &fingerprint {
                    return Ok(stopped(&request, "rejected", "TOOL_IDEMPOTENCY_CONFLICT"));
                }
                return Ok(json!({"envelope":value}));
            }
        }
        let timeout = contract["timeout_seconds"]
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.)
            .unwrap_or(0.)
            .min(remaining - started.elapsed().as_secs_f64());
        if timeout <= 0. {
            if claimed {
                let _=self.store("ambiguous",&json!({"actor_id":request["actor_id"],"tool_name":request["tool_name"],"idempotency_key":key(&request),"fingerprint":fingerprint}));
            }
            return Ok(stopped(&request, "timeout", "TOOL_DEADLINE_EXCEEDED"));
        }
        if self.pending.len() >= 256 {
            return Err("TOOL_EXECUTION_STATE_ERROR");
        }
        let ticket = identifier()?;
        self.pending.insert(
            ticket.clone(),
            Pending {
                request,
                contract,
                fingerprint,
                claimed,
                deadline: Instant::now() + Duration::from_secs_f64(timeout.min(86400.)),
            },
        );
        Ok(json!({"ticket":ticket,"timeout_seconds":timeout}))
    }
    fn ambiguous(&self, pending: &Pending) {
        if pending.claimed {
            let _=self.store("ambiguous",&json!({"actor_id":pending.request["actor_id"],"tool_name":pending.request["tool_name"],"idempotency_key":key(&pending.request),"fingerprint":pending.fingerprint}));
        }
    }
    fn finish(&mut self, ticket: &str, raw: &Value, failure: Option<&str>) -> Result<Value> {
        let pending = self
            .pending
            .remove(ticket)
            .ok_or("TOOL_EXECUTION_STATE_ERROR")?;
        let request = &pending.request;
        let contract = &pending.contract;
        let fail = |status, code| {
            self.ambiguous(&pending);
            json!({"envelope":envelope(request,status,code,json!({}))})
        };
        if failure == Some("timeout") || Instant::now() > pending.deadline {
            return Ok(fail("timeout", "TOOL_DEADLINE_EXCEEDED"));
        }
        if let Some(failure) = failure {
            return Ok(fail(
                "error",
                if failure == "invalid" {
                    "TOOL_RESULT_INVALID"
                } else {
                    "TOOL_HANDLER_ERROR"
                },
            ));
        }
        if !raw.is_object() {
            return Ok(fail("error", "TOOL_RESULT_INVALID"));
        }
        if raw["schema"] != contract["result_schema"] {
            return Ok(fail("error", "TOOL_RESULT_SCHEMA_MISMATCH"));
        }
        if raw["tool"] != contract["name"]
            || contract["result_schema"] == "les_tool_result_v1" && !valid_result(raw)
        {
            return Ok(fail("error", "TOOL_RESULT_INVALID"));
        }
        let encoded = match canonical::encode(raw) {
            Ok(value) => value,
            Err(_) => return Ok(fail("error", "TOOL_RESULT_INVALID")),
        };
        let items = item_count(raw);
        let budget = (|| -> Result<Value> {
            Ok(
                if encoded.chars().count()
                    <= contract["result_budget"]["max_chars"].as_u64().unwrap_or(0) as usize
                    && items
                        <= contract["result_budget"]["max_items"].as_u64().unwrap_or(0) as usize
                {
                    let mut effective = raw;
                    for _ in 0..8 {
                        if effective["tool"]
                            .as_str()
                            .is_some_and(|v| v.starts_with("use_extensions_"))
                            && effective["operation"] == "call"
                            && effective["result"]["schema"] == "les_tool_result_v1"
                        {
                            effective = &effective["result"];
                        } else {
                            break;
                        }
                    }
                    if matches!(
                        raw["status"].as_str(),
                        Some("error" | "missing" | "blocked")
                    ) || matches!(
                        effective["status"].as_str(),
                        Some("error" | "missing" | "blocked")
                    ) {
                        let code = effective["result"]["code"]
                            .as_str()
                            .filter(|v| {
                                !v.is_empty()
                                    && v.len() <= 100
                                    && v.bytes().all(|c| {
                                        c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'
                                    })
                            })
                            .unwrap_or("TOOL_REPORTED_ERROR");
                        envelope(request, "error", code, raw.clone())
                    } else {
                        envelope(request, "ok", "TOOL_OK", raw.clone())
                    }
                } else {
                    let cursor = format!(
                        "tool-result:{}:{}",
                        request["call_id"].as_str().unwrap_or(""),
                        identifier()?
                    );
                    if self.database.is_some() {
                        self.store("put_cursor",&json!({"cursor":cursor,"actor_id":request["actor_id"],"expires_at_epoch":now()+self.ttl,"result":raw}))?;
                    } else {
                        self.cursors.push_back(Cursor {
                            key: cursor.clone(),
                            actor: request["actor_id"].as_str().unwrap_or("").into(),
                            expires: Instant::now() + Duration::from_secs_f64(self.ttl),
                            raw: raw.clone(),
                        });
                        while self.cursors.len() > self.capacity {
                            self.cursors.pop_front();
                        }
                    }
                    let mut value = envelope(
                        request,
                        "overflow",
                        "TOOL_RESULT_BUDGET_EXCEEDED",
                        json!({"schema":contract["result_schema"],"reference":cursor,"whole_result_stored":true}),
                    );
                    value["cursor"] = cursor.into();
                    value["omitted_items"] = json!(items.saturating_sub(
                        contract["result_budget"]["max_items"].as_u64().unwrap_or(0) as usize
                    ));
                    value
                },
            )
        })();
        let mut value = match budget {
            Ok(value) => value,
            Err(_) => {
                self.ambiguous(&pending);
                return Ok(stopped(request, "error", "TOOL_RESULT_INVALID"));
            }
        };
        if !key(request).is_empty() {
            if pending.claimed {
                if self.store("complete",&json!({"actor_id":request["actor_id"],"tool_name":request["tool_name"],"idempotency_key":key(request),"fingerprint":pending.fingerprint,"envelope":value})).is_err(){self.ambiguous(&pending);value=envelope(request,"error","TOOL_EXECUTION_STATE_ERROR",json!({}));}
            } else {
                self.idempotency.insert(
                    (
                        request["actor_id"].as_str().unwrap_or("").into(),
                        key(request).into(),
                    ),
                    (pending.fingerprint, value.clone()),
                );
            }
        }
        Ok(json!({"envelope":value}))
    }
}
fn scope_escapes(contract: &Value, request: &Value, resolved: &Value) -> bool {
    let has = |name: &str| {
        contract["scopes"]
            .as_array()
            .is_some_and(|v| v.contains(&json!(name)))
    };
    if has("chat_attachment") {
        let attachment = request["arguments"]["attachment_id"]
            .as_str()
            .unwrap_or("")
            .trim();
        if attachment.is_empty()
            || !request["allowed_attachment_ids"]
                .as_array()
                .is_some_and(|v| {
                    v.iter()
                        .any(|v| v.as_str().is_some_and(|v| v.trim() == attachment))
                })
        {
            return true;
        }
    }
    if !has("dataset") {
        return false;
    }
    let allowed = request["allowed_dataset_ids"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_default();
    if allowed.contains("*") {
        return false;
    }
    let mut requested = HashSet::new();
    let mut queue = vec![&request["arguments"]];
    while let Some(value) = queue.pop() {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "dataset_id" {
                        if !value.is_string()
                            && !value.is_null()
                            && value != false
                            && value != 0
                            && *value != json!([])
                            && *value != json!({})
                        {
                            return true;
                        }
                        if let Some(id) = value.as_str().filter(|s| !s.is_empty()) {
                            requested.insert(id);
                        }
                    } else if key == "dataset_ids" {
                        if let Some(values) = value.as_array() {
                            if values.iter().any(|v| !v.is_string()) {
                                return true;
                            }
                            requested.extend(
                                values
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .filter(|s| !s.is_empty()),
                            );
                        }
                    } else {
                        queue.push(value);
                    }
                }
            }
            Value::Array(values) => queue.extend(values),
            _ => {}
        }
    }
    if let Some(values) = resolved.as_array() {
        requested.extend(
            values
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty()),
        );
    }
    if requested.is_empty() {
        !has("chat_attachment")
    } else {
        requested.iter().any(|v| !allowed.contains(v))
    }
}
fn item_count(value: &Value) -> usize {
    match value {
        Value::Object(map) => map.values().map(item_count).sum(),
        Value::Array(values) => values.len() + values.iter().map(item_count).sum::<usize>(),
        _ => 0,
    }
}
fn valid_result(value: &Value) -> bool {
    if ["tool", "operation", "trace", "status"]
        .iter()
        .any(|k| !value[*k].as_str().is_some_and(|s| !s.is_empty()))
        || !value["result"].is_object()
        || ["inputs", "sources", "missing", "warnings"]
            .iter()
            .any(|k| !value[*k].is_array())
        || !value["decision_required_from_model"].is_boolean()
    {
        return false;
    }
    if value["decisions"].as_array().is_some_and(|v| {
        v.iter().any(|v| {
            matches!(
                v.as_str(),
                Some(
                    "select_work_final"
                        | "select_norm_final"
                        | "select_contract_quantity"
                        | "missing_as_zero"
                        | "candidate_as_selected"
                )
            )
        })
    }) {
        return false;
    }
    matches!(
        value["status"].as_str(),
        Some("ok" | "missing" | "blocked" | "error")
    )
}
static EXECUTORS: OnceLock<Mutex<HashMap<u64, Arc<Mutex<Executor>>>>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
pub fn execute(request: &Value) -> Value {
    let run = || -> Result<Value> {
        if request["schema"] != "les.tool-execution.v1" {
            return Err("TOOL_EXECUTION_STATE_ERROR");
        }
        let action = request["action"]
            .as_str()
            .ok_or("TOOL_EXECUTION_STATE_ERROR")?;
        if action == "store" {
            return store::execute(
                request["database"]
                    .as_str()
                    .ok_or("TOOL_EXECUTION_STATE_ERROR")?,
                request["store_action"]
                    .as_str()
                    .ok_or("TOOL_EXECUTION_STATE_ERROR")?,
                &request["arguments"],
            );
        }
        if action == "hash" {
            return Ok(
                json!({"sha256":crate::sha256_hex(canonical::encode(&request["arguments"])?.as_bytes()),"canonical":canonical::encode(&request["arguments"])?}),
            );
        }
        let states = EXECUTORS.get_or_init(|| Mutex::new(HashMap::new()));
        if action == "new" {
            let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let ttl = request["cursor_ttl_seconds"]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0. && *v <= 86400.)
                .unwrap_or(300.);
            let database = request["database"].as_str().map(str::to_owned);
            if let Some(database) = &database {
                store::execute(database, "ensure", &json!({}))?;
            }
            let mut states = states.lock().map_err(|_| "TOOL_EXECUTION_STATE_ERROR")?;
            if states.len() >= 1024 {
                return Err("TOOL_EXECUTION_STATE_ERROR");
            }
            states.insert(
                id,
                Arc::new(Mutex::new(Executor {
                    closed: false,
                    database,
                    ttl,
                    capacity: request["cursor_capacity"].as_u64().unwrap_or(128).min(1024) as usize,
                    idempotency: HashMap::new(),
                    cursors: VecDeque::new(),
                    pending: HashMap::new(),
                })),
            );
            return Ok(json!({"handle":id}));
        }
        let handle = request["handle"]
            .as_u64()
            .ok_or("TOOL_EXECUTION_STATE_ERROR")?;
        if action == "close" {
            let state = states
                .lock()
                .map_err(|_| "TOOL_EXECUTION_STATE_ERROR")?
                .remove(&handle);
            if let Some(state) = state {
                let mut state = state.lock().map_err(|_| "TOOL_EXECUTION_STATE_ERROR")?;
                state.closed = true;
                for pending in state.pending.values() {
                    state.ambiguous(pending);
                }
                state.pending.clear();
            }
            return Ok(Value::Null);
        }
        let state = states
            .lock()
            .map_err(|_| "TOOL_EXECUTION_STATE_ERROR")?
            .get(&handle)
            .cloned()
            .ok_or("TOOL_EXECUTION_STATE_ERROR")?;
        let mut state = state.lock().map_err(|_| "TOOL_EXECUTION_STATE_ERROR")?;
        if state.closed {
            return Err("TOOL_EXECUTION_STATE_ERROR");
        }
        match action {
            "begin" => state.begin(
                request["request"].clone(),
                request["contract"].clone(),
                &request["admission"],
            ),
            "finish" => state.finish(
                request["ticket"]
                    .as_str()
                    .ok_or("TOOL_EXECUTION_STATE_ERROR")?,
                &request["raw"],
                request["failure"].as_str(),
            ),
            "cursor" => state.cursor(
                request["cursor"]
                    .as_str()
                    .ok_or("TOOL_EXECUTION_STATE_ERROR")?,
                request["actor_id"]
                    .as_str()
                    .ok_or("TOOL_EXECUTION_STATE_ERROR")?,
            ),
            _ => Err("TOOL_EXECUTION_STATE_ERROR"),
        }
    };
    match run() {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(code) => json!({"ok":false,"code":code}),
    }
}
