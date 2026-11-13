//! LES admission policy; it does not schedule model/tool turns.
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashSet},
    time::Instant,
};
pub(super) struct Policy {
    pub allowed: BTreeSet<String>,
    pub remaining: u64,
    pub deadline: Instant,
    pub closed: bool,
    pub reason: String,
    pub failed: Option<String>,
    pub results: Vec<Value>,
    pub rounds: Vec<Value>,
    pub epochs: Value,
    pub binding: Value,
    pub accepted: Option<Value>,
    pub model_messages: Vec<Value>,
    pub calls: std::collections::HashMap<String, Value>,
    pub data_result_ids: HashSet<String>,
    pub private_ids: HashSet<String>,
    pub completed: std::collections::HashMap<String, u64>,
    pub failed_calls: HashSet<String>,
    pub batch: u64,
    pub sequence: u64,
    pub workspace_epoch: u64,
    pub extension_epoch: u64,
    pub completion_revised: bool,
    pub answer_window_prepared: bool,
    pub empty_answers: u8,
}
impl Policy {
    pub fn private_id(&mut self, kind: &str) -> String {
        loop {
            self.sequence += 1;
            let id = format!("les-{kind}-{}", self.sequence);
            if !self.calls.contains_key(&id) && self.private_ids.insert(id.clone()) {
                return id;
            }
        }
    }
    pub fn check(&self) -> Result<(), String> {
        if let Some(code) = &self.failed {
            return Err(code.clone());
        }
        if Instant::now() >= self.deadline {
            return Err("RESEARCH_DEADLINE_EXCEEDED".into());
        }
        Ok(())
    }
    pub fn coverage(&self) -> Result<Value, String> {
        crate::harness_evidence::execute(
            &json!({"action":"pdf_read_coverage","results":self.results}),
        )
        .map_err(str::to_owned)
    }
    pub fn identity(&self, call: &Value) -> String {
        let base=json!({"tool":call["tool"],"args":call.get("args").cloned().unwrap_or_else(||json!({}))}).to_string();
        let name = call["tool"].as_str().unwrap_or("");
        let args = &call["args"];
        let workspace_write = |args: &Value| {
            args["operation"]
                .as_str()
                .is_some_and(|op| ["save", "edit", "delete", "restore"].contains(&op))
        };
        if (name == "agent_workspace" && workspace_write(args))
            || (name.starts_with("use_extensions_")
                && args["operation"] == "call"
                && args["name"] == "agent_workspace"
                && workspace_write(&args["arguments"]))
        {
            // A write already declares its expected revision. Advancing read
            // epochs must not authorize the same immutable effect twice.
            return format!("effect:{base}");
        }
        if name == "agent_workspace" {
            format!("agent:{}:{base}", self.workspace_epoch)
        } else if let Some(epoch) = self.epochs.get(name) {
            format!("epoch:{epoch}:{base}")
        } else {
            base
        }
    }
    pub fn invalidate_extensions(&mut self) {
        self.extension_epoch = self.extension_epoch.saturating_add(1);
        for name in &self.allowed {
            if name.starts_with("use_extensions_") {
                self.epochs[name] = json!(self.extension_epoch);
            }
        }
    }
    pub fn epochs(&mut self, epochs: Value) -> Result<(), &'static str> {
        let values = epochs
            .as_object()
            .filter(|v| v.len() <= 128)
            .ok_or("AGENT_CONTEXT_INVALID")?;
        if values.keys().any(|n| !self.allowed.contains(n)) || epochs.to_string().len() > 16384 {
            return Err("AGENT_CONTEXT_INVALID");
        }
        self.epochs = epochs;
        Ok(())
    }
    pub fn record(&mut self, payload: Value) -> Result<(), String> {
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
        let changed = effect["tool"] == "agent_workspace"
            && (effect["execution"]["persisted"] == true
                || effect["result"]["changed"] == true
                || effect["result"]["code"] == "AGENT_CONFLICT");
        let table = effect["tool"] == "table_document"
            && effect["status"] == "ok"
            && effect["result"]["revision"].is_u64()
            && self.epochs["table_document"] != effect["result"]["revision"];
        if changed {
            self.workspace_epoch = self.workspace_epoch.saturating_add(1);
        }
        if table {
            self.epochs["table_document"] = effect["result"]["revision"].clone();
        }
        if changed || table {
            self.invalidate_extensions();
        }
        let outcome = crate::harness_evidence::execute(
            &json!({"action":"source_read_outcome","result":payload}),
        )
        .map_err(str::to_owned)?;
        self.results.push(payload);
        if outcome["allowed"] != true {
            let code = outcome["code"]
                .as_str()
                .unwrap_or("SOURCE_READ_FAILED")
                .to_owned();
            self.failed = Some(code.clone());
            return Err(code);
        }
        Ok(())
    }
    pub fn parse(&mut self, response: &Value, final_answer: bool) -> Result<Vec<Value>, String> {
        if !response.is_object() {
            return Err("UPSTREAM_RESPONSE_INVALID".into());
        }
        if response
            .get("tool_calls")
            .filter(|v| !v.is_null())
            .is_some_and(|v| !v.is_array())
        {
            return Err("UPSTREAM_TOOL_CALLS_INVALID".into());
        }
        let mut calls = Vec::new();
        let mut ids = HashSet::new();
        if let Some(raw) = response["tool_calls"].as_array().filter(|v| !v.is_empty()) {
            for call in raw {
                let f = &call["function"];
                let name = f["name"].as_str().ok_or("UPSTREAM_TOOL_CALLS_INVALID")?;
                if !self.allowed.contains(name) {
                    return Err("TOOL_NOT_ALLOWED".into());
                }
                let a = f.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let a = if let Some(s) = a.as_str() {
                    serde_json::from_str(s).map_err(|_| "INVALID_NATIVE_TOOL_ARGUMENTS")?
                } else {
                    a
                };
                if !a.is_object() {
                    return Err("INVALID_NATIVE_TOOL_ARGUMENTS".into());
                }
                let mut c = json!({"tool":name,"args":a});
                if let Some(id) = call.get("id") {
                    if !id.as_str().is_some_and(|s| {
                        !s.is_empty() && !self.private_ids.contains(s) && ids.insert(s.to_owned())
                    }) {
                        return Err("UPSTREAM_TOOL_CALLS_INVALID".into());
                    }
                    c["call_id"] = id.clone();
                }
                calls.push(c);
            }
        } else if let Some(text) = response["selection_text"].as_str() {
            let parsed=crate::harness_evidence::execute(&json!({"action":"parse_tool_calls","text":text,"allowed":self.allowed,"max_calls":self.remaining.max(1)})).map_err(str::to_owned)?;
            calls = parsed["calls"]
                .as_array()
                .ok_or("UPSTREAM_TOOL_CALLS_INVALID")?
                .clone();
        }
        if self.binding.is_object() {
            for call in &mut calls {
                *call=crate::harness_evidence::execute(&json!({"action":"augment_tool_args","call":call,"question":self.binding["question"],"dataset_ids":self.binding["dataset_ids"],"target":self.binding["target"],"sources":self.binding["sources"]})).map_err(str::to_owned)?;
            }
        }
        if final_answer && !calls.is_empty() {
            return Err("TOOL_BUDGET_EXHAUSTED".into());
        }
        Ok(calls)
    }
}
