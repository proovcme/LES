//! Request-owned model/tool coordinator. Executors remain responsible for their effects and scope.
mod answer;
mod runner;
pub use runner::run_research;
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};

#[derive(Debug)]
pub enum Step {
    Model { final_answer: bool },
    Tool(Value),
    Complete(Value),
    Failed(String),
}
enum Waiting {
    None,
    Model(bool),
    Tool(String),
}
pub struct AgentLoop {
    control: Value,
    allowed: BTreeSet<String>,
    queue: VecDeque<Value>,
    results: Vec<Value>,
    waiting: Waiting,
    final_requested: bool,
    terminal: Option<Value>,
    failed: Option<String>,
    workspace_epoch: u64,
    extension_epoch: u64,
    epochs: Value,
    pub rounds: Vec<Value>,
}
impl AgentLoop {
    pub fn new(
        max_calls: u64,
        deadline: f64,
        allowed: impl IntoIterator<Item = String>,
    ) -> Result<Self, &'static str> {
        let control = crate::harness::execute(
            &json!({"action":"new","max_calls":max_calls,"deadline_seconds":deadline}),
        )?["state"]
            .clone();
        Ok(Self {
            control,
            allowed: allowed.into_iter().collect(),
            queue: VecDeque::new(),
            results: Vec::new(),
            waiting: Waiting::None,
            final_requested: false,
            terminal: None,
            failed: None,
            workspace_epoch: 0,
            extension_epoch: 0,
            epochs: json!({}),
            rounds: Vec::new(),
        })
    }
    fn control(&mut self, action: &str, extra: Value) -> Result<Value, &'static str> {
        let mut request = json!({"action":action,"state":self.control});
        request
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap_or_default());
        let result = crate::harness::execute(&request)?;
        self.control = result["state"].clone();
        Ok(result)
    }
    fn identity(&self, call: &Value) -> String {
        let base=json!({"tool":call["tool"],"args":call.get("args").cloned().unwrap_or_else(||json!({}))}).to_string();
        if call["tool"] == "agent_workspace" {
            format!("agent:{}:{base}", self.workspace_epoch)
        } else if let Some(epoch) = call["tool"].as_str().and_then(|name| self.epochs.get(name)) {
            format!("epoch:{epoch}:{base}")
        } else {
            base
        }
    }
    pub fn results(&self) -> &[Value] {
        &self.results
    }
    pub fn set_epochs(&mut self, epochs: Value) -> Result<(), &'static str> {
        let values = epochs
            .as_object()
            .filter(|values| values.len() <= 128)
            .ok_or("AGENT_CONTEXT_INVALID")?;
        if values.keys().any(|name| !self.allowed.contains(name))
            || epochs.to_string().len() > 16384
        {
            return Err("AGENT_CONTEXT_INVALID");
        }
        self.epochs = epochs;
        Ok(())
    }
    pub fn update_epochs(&mut self, epochs: Value) -> Result<(), &'static str> {
        let mut combined = self.epochs.clone();
        combined
            .as_object_mut()
            .ok_or("AGENT_CONTEXT_INVALID")?
            .extend(epochs.as_object().ok_or("AGENT_CONTEXT_INVALID")?.clone());
        self.set_epochs(combined)
    }
    pub fn remaining(&self) -> u64 {
        self.control["remaining"].as_u64().unwrap_or(0)
    }
    pub fn stop_reason(&self) -> &str {
        self.control["reason"].as_str().unwrap_or("unknown")
    }
    pub fn fail(&mut self, code: impl Into<String>) {
        self.failed = Some(code.into());
        self.waiting = Waiting::None;
        self.queue.clear();
    }
    fn coverage(&self) -> Result<Value, &'static str> {
        crate::harness_evidence::execute(
            &json!({"action":"pdf_read_coverage","results":self.results}),
        )
    }
    fn closing(&mut self) -> Result<Step, &'static str> {
        let coverage = self.coverage()?;
        if coverage["complete"] != true {
            let code = coverage["code"]
                .as_str()
                .unwrap_or("PDF_PAGE_INCOMPLETE")
                .to_string();
            self.fail(code.clone());
            return Ok(Step::Failed(code));
        }
        if let Some(value) = &self.terminal {
            return Ok(Step::Complete(value.clone()));
        }
        if self.final_requested {
            self.fail("MODEL_EMPTY_RESPONSE");
            return Ok(Step::Failed("MODEL_EMPTY_RESPONSE".into()));
        }
        self.final_requested = true;
        self.waiting = Waiting::Model(true);
        Ok(Step::Model { final_answer: true })
    }
    pub fn next(&mut self) -> Result<Step, &'static str> {
        if let Some(code) = &self.failed {
            return Ok(Step::Failed(code.clone()));
        }
        if !matches!(self.waiting, Waiting::None) {
            return Err("AGENT_OPERATION_INFLIGHT");
        }
        while let Some(call) = self.queue.pop_front() {
            let started = self.control("begin", json!({"identity":self.identity(&call)}))?;
            if started["allowed"] == true {
                if let Some(round) = self.rounds.last_mut() {
                    round["executed"] = json!(round["executed"].as_u64().unwrap_or(0) + 1);
                }
                self.waiting = Waiting::Tool(call["tool"].as_str().unwrap_or("").into());
                return Ok(Step::Tool(call));
            }
        }
        if self.terminal.is_some() || self.control["stopped"] == true {
            return self.closing();
        }
        if self.control("round", json!({}))?["allowed"] != true {
            return self.closing();
        }
        let coverage = self.coverage()?;
        if let Some(call) = coverage.get("continuation").filter(|v| v.is_object()) {
            if self.allowed.contains("read_pdf_source") {
                self.rounds.push(json!({"round":self.control["round"],"proposed":1,"executed":0,"model_sent":false,"continuation":"native_pdf_cursor"}));
                self.queue.push_back(call.clone());
                return self.next();
            }
            self.fail("PDF_PAGE_INCOMPLETE");
            return Ok(Step::Failed("PDF_PAGE_INCOMPLETE".into()));
        }
        self.waiting = Waiting::Model(false);
        Ok(Step::Model {
            final_answer: false,
        })
    }
    pub fn model_result(&mut self, response: Value) -> Result<(), &'static str> {
        self.model_result_bound(response, None)
    }
    pub fn model_result_bound(
        &mut self,
        response: Value,
        binding: Option<&Value>,
    ) -> Result<(), &'static str> {
        let final_answer = match self.waiting {
            Waiting::Model(value) => value,
            _ => return Err("AGENT_MODEL_NOT_REQUESTED"),
        };
        self.waiting = Waiting::None;
        if !response.is_object() {
            return Err("UPSTREAM_RESPONSE_INVALID");
        }
        let mut calls = Vec::new();
        if response
            .get("tool_calls")
            .filter(|v| !v.is_null())
            .is_some_and(|v| !v.is_array())
        {
            self.fail("UPSTREAM_TOOL_CALLS_INVALID");
            return Err("UPSTREAM_TOOL_CALLS_INVALID");
        }
        let raw = response
            .get("tool_calls")
            .filter(|v| !v.is_null())
            .and_then(Value::as_array);
        if let Some(raw) = raw.filter(|calls| !calls.is_empty()) {
            for call in raw {
                let function = &call["function"];
                let name = function["name"]
                    .as_str()
                    .ok_or("UPSTREAM_TOOL_CALLS_INVALID")?;
                if !self.allowed.contains(name) {
                    self.fail("TOOL_NOT_ALLOWED");
                    return Ok(());
                }
                let args = function
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let args = if let Some(raw) = args.as_str() {
                    serde_json::from_str(raw).map_err(|_| "INVALID_NATIVE_TOOL_ARGUMENTS")?
                } else {
                    args
                };
                if !args.is_object() {
                    return Err("INVALID_NATIVE_TOOL_ARGUMENTS");
                }
                let mut value = json!({"tool":name,"args":args});
                if let Some(id) = call.get("id") {
                    value["call_id"] = id.clone();
                }
                calls.push(value);
            }
        } else if let Some(selection) = response["selection_text"].as_str() {
            let parsed = crate::harness_evidence::execute(
                &json!({"action":"parse_tool_calls","text":selection,
                "allowed":self.allowed,"max_calls":self.remaining().max(1)}),
            )?;
            calls = parsed["calls"]
                .as_array()
                .ok_or("UPSTREAM_TOOL_CALLS_INVALID")?
                .clone();
        }
        if let Some(binding) = binding {
            for call in &mut calls {
                *call = crate::harness_evidence::execute(
                    &json!({"action":"augment_tool_args","call":call,
                    "question":binding["question"],"dataset_ids":binding["dataset_ids"],"target":binding["target"],"sources":binding["sources"]}),
                )?;
            }
        }
        if final_answer && !calls.is_empty() {
            self.fail("TOOL_BUDGET_EXHAUSTED");
            return Ok(());
        }
        if calls.is_empty() {
            self.control("select", json!({"identities":[]}))?;
            if response["text"]
                .as_str()
                .is_some_and(|v| !v.trim().is_empty())
                && raw.is_none_or(Vec::is_empty)
            {
                self.terminal = Some(response);
            }
        } else {
            let selected = self.control(
                "select",
                json!({"identities":calls.iter().map(|v|self.identity(v)).collect::<Vec<_>>()}),
            )?;
            self.queue = selected["indexes"]
                .as_array()
                .ok_or("HARNESS_STATE_INVALID")?
                .iter()
                .map(|v| calls[v.as_u64().unwrap() as usize].clone())
                .collect();
        }
        self.rounds.push(json!({"round":self.control["round"],"proposed":calls.len(),"executed":0,"model_sent":true}));
        Ok(())
    }
    pub fn tool_result(&mut self, payload: Value) -> Result<(), &'static str> {
        let Waiting::Tool(expected) = &self.waiting else {
            return Err("AGENT_TOOL_NOT_REQUESTED");
        };
        if payload["tool"] != expected.as_str() {
            self.fail("TOOL_RESULT_MISMATCH");
            return Err("TOOL_RESULT_MISMATCH");
        }
        self.waiting = Waiting::None;
        let failed = matches!(
            payload["status"].as_str(),
            Some("error" | "missing" | "blocked")
        ) || payload["ok"] == false;
        self.control("finish", json!({"failed":failed}))?;
        let mut effect = &payload;
        for _ in 0..8 {
            if effect["tool"]
                .as_str()
                .is_some_and(|s| s.starts_with("use_extensions_"))
                && effect["operation"] == "call"
                && effect["result"]["schema"] == "les_tool_result_v1"
            {
                effect = &effect["result"];
            } else {
                break;
            }
        }
        let workspace_changed = effect["tool"] == "agent_workspace"
            && (effect["execution"]["persisted"] == true
                || effect["result"]["changed"] == true
                || effect["result"]["code"] == "AGENT_CONFLICT");
        let table_changed = effect["tool"] == "table_document"
            && effect["status"] == "ok"
            && effect["result"]["revision"].is_u64()
            && self.epochs["table_document"] != effect["result"]["revision"];
        if workspace_changed {
            self.workspace_epoch = self.workspace_epoch.saturating_add(1);
        }
        if table_changed {
            self.epochs["table_document"] = effect["result"]["revision"].clone();
        }
        if workspace_changed || table_changed {
            self.extension_epoch = self.extension_epoch.saturating_add(1);
            for name in &self.allowed {
                if name.starts_with("use_extensions_") {
                    self.epochs[name] = json!(self.extension_epoch);
                }
            }
        }
        let outcome = crate::harness_evidence::execute(
            &json!({"action":"source_read_outcome","result":payload}),
        )?;
        self.results.push(payload);
        if outcome["allowed"] != true {
            self.fail(outcome["code"].as_str().unwrap_or("SOURCE_READ_FAILED"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn propose(agent: &mut AgentLoop, args: Value) {
        assert!(matches!(
            agent.next().unwrap(),
            Step::Model {
                final_answer: false
            }
        ));
        agent.model_result(json!({"tool_calls":[{"id":"call","function":{"name":"use_extensions_fixture","arguments":args}}]})).unwrap();
        assert!(matches!(agent.next().unwrap(), Step::Tool(_)));
    }
    fn extension(inner: Value) -> Value {
        json!({"tool":"use_extensions_fixture","operation":"call","status":"ok","result":inner})
    }
    #[test]
    fn wrapped_workspace_write_invalidates_prior_read_identity_and_survives_table_epoch_update() {
        let mut agent = AgentLoop::new(
            8,
            30.,
            [
                "use_extensions_fixture".into(),
                "agent_workspace".into(),
                "table_document".into(),
            ],
        )
        .unwrap();
        let read =
            json!({"operation":"call","name":"agent_workspace","arguments":{"operation":"read"}});
        propose(&mut agent, read.clone());
        agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"agent_workspace","status":"ok","result":{"changed":false}}))).unwrap();
        propose(
            &mut agent,
            json!({"operation":"call","name":"agent_workspace","arguments":{"operation":"save"}}),
        );
        agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"agent_workspace","status":"ok","result":{"changed":true}}))).unwrap();
        agent.update_epochs(json!({"table_document":7})).unwrap();
        propose(&mut agent, read);
    }
    #[test]
    fn wrapped_table_commit_allows_reading_the_next_packet() {
        let mut agent = AgentLoop::new(
            8,
            30.,
            ["use_extensions_fixture".into(), "table_document".into()],
        )
        .unwrap();
        agent.update_epochs(json!({"table_document":0})).unwrap();
        let next =
            json!({"operation":"call","name":"table_document","arguments":{"operation":"next"}});
        propose(&mut agent, next.clone());
        agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"table_document","status":"ok","result":{"revision":0}}))).unwrap();
        propose(
            &mut agent,
            json!({"operation":"call","name":"table_document","arguments":{"operation":"record"}}),
        );
        agent.tool_result(extension(json!({"schema":"les_tool_result_v1","tool":"table_document","status":"ok","result":{"revision":1}}))).unwrap();
        agent.update_epochs(json!({"table_document":1})).unwrap();
        propose(&mut agent, next);
    }
    #[test]
    fn failed_file_read_never_requests_a_confident_followup_model_answer() {
        let mut agent = AgentLoop::new(4, 30., ["filesystem_read_text".into()]).unwrap();
        assert!(matches!(agent.next().unwrap(), Step::Model { .. }));
        agent.model_result(json!({"text":"","tool_calls":[{"id":"file-1","function":{"name":"filesystem_read_text","arguments":{"path":"../outside.txt"}}}]})).unwrap();
        assert!(matches!(agent.next().unwrap(), Step::Tool(_)));
        agent.tool_result(json!({"tool":"filesystem_read_text","status":"error","result":{"code":"FILESYSTEM_SCOPE_DENIED"}})).unwrap();
        assert!(
            matches!(agent.next().unwrap(),Step::Failed(code) if code=="FILESYSTEM_SCOPE_DENIED")
        );
    }
    #[test]
    fn a_read_failure_never_requests_a_successful_final_answer() {
        let mut agent = AgentLoop::new(4, 30., ["read_pdf_source".into()]).unwrap();
        assert!(matches!(
            agent.next().unwrap(),
            Step::Model {
                final_answer: false
            }
        ));
        agent.model_result(json!({"text":"","tool_calls":[{"function":{"name":"read_pdf_source","arguments":"{\"page\":3}"}}]})).unwrap();
        assert!(matches!(agent.next().unwrap(), Step::Tool(_)));
        agent.tool_result(json!({"tool":"read_pdf_source","status":"error","result":{"code":"PDF_REVISION_CHANGED"}})).unwrap();
        assert!(matches!(agent.next().unwrap(),Step::Failed(code) if code=="PDF_REVISION_CHANGED"));
        assert_eq!(agent.results().len(), 1);
    }
    #[test]
    fn duplicate_calls_are_not_executed_twice_and_results_survive_budget_end() {
        let mut agent = AgentLoop::new(1, 30., ["search_sources".into()]).unwrap();
        agent.next().unwrap();
        agent.model_result(json!({"tool_calls":[{"function":{"name":"search_sources","arguments":"{\"q\":\"test\"}"}},{"function":{"name":"search_sources","arguments":"{\"q\":\"test\"}"}}]})).unwrap();
        assert!(matches!(agent.next().unwrap(), Step::Tool(_)));
        agent
            .tool_result(json!({"tool":"search_sources","status":"ok","result":{}}))
            .unwrap();
        assert!(matches!(
            agent.next().unwrap(),
            Step::Model { final_answer: true }
        ));
        agent
            .model_result(json!({"text":"answer","tool_calls":[]}))
            .unwrap();
        assert!(matches!(agent.next().unwrap(), Step::Complete(_)));
        assert_eq!(agent.remaining(), 0);
    }
    #[test]
    fn incomplete_pages_are_read_by_exact_cursor_before_calling_the_model_again() {
        let mut agent = AgentLoop::new(3, 30., ["read_pdf_source".into()]).unwrap();
        agent.next().unwrap();
        agent.model_result(json!({"tool_calls":[{"function":{"name":"read_pdf_source","arguments":{"page":1}}}]})).unwrap();
        agent.next().unwrap();
        agent.tool_result(json!({"tool":"read_pdf_source","status":"ok","result":{"doc_id":"doc","dataset_id":"ds","doc_name":"test.pdf","source_sha256":"hash","page":1,"offset":0,"total_chars":4,"text":"ab","complete":false}})).unwrap();
        let Step::Tool(call) = agent.next().unwrap() else {
            panic!("Model was called before completing the page")
        };
        assert_eq!(call["args"]["offset"], 2);
        assert_eq!(call["args"]["source_sha256"], "hash");
    }
}
