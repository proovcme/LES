//! Rho is the only model/tool scheduler. Host events are I/O and validation ports.
use super::policy::Policy;
use super::{Decision, Envelope, Step};
use rho_sdk::{
    model::{ContentBlock, ModelIdentity, ModelRequest, ModelResponse, ToolCall, ToolSpec},
    provider::{ModelProvider, ProviderFuture},
    tool::{Tool, ToolContext, ToolError, ToolErrorKind, ToolFuture, ToolInvocation, ToolOutput},
    ProviderError, ProviderErrorKind, Retryability,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
pub(super) const CONTROL: &str = "les_internal_completion_gate";
#[derive(Clone)]
pub(super) struct Host {
    pub policy: Arc<Mutex<Policy>>,
    pub events: mpsc::UnboundedSender<Envelope>,
}
impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LES Rho host")
    }
}
impl Host {
    pub async fn ask(&self, step: Step) -> Result<Decision, String> {
        self.policy.lock().unwrap().check()?;
        let (tx, rx) = oneshot::channel();
        self.events
            .send(Envelope {
                step,
                reply: Some(tx),
            })
            .map_err(|_| "CHAT_CANCELLED")?;
        rx.await.map_err(|_| "CHAT_CANCELLED".into())
    }
    fn control(&self, kind: &str, extra: Value) -> ModelResponse {
        let mut p = self.policy.lock().unwrap();
        let id = p.private_id("control");
        ModelResponse::Assistant(vec![ContentBlock::ToolCall(ToolCall {
            id,
            name: CONTROL.into(),
            arguments: json!({"kind":kind,"value":extra}),
        })])
    }
    pub fn abort(&self, code: &str) {
        self.policy.lock().unwrap().failed = Some(code.into());
    }
}
fn fault(code: impl Into<String>) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Other,
        code.into(),
        Retryability::Permanent,
    )
}
fn model_history(
    messages: &[rho_sdk::model::Message],
    data_result_ids: &std::collections::HashSet<String>,
) -> Vec<Value> {
    use rho_sdk::model::Message;
    let mut result = Vec::new();
    let mut hidden = std::collections::HashSet::new();
    for message in messages {
        if let Some(blocks) = message.completed_assistant_content() {
            let mut calls = Vec::new();
            for block in blocks {
                if let ContentBlock::ToolCall(c) = block {
                    if c.name == CONTROL || data_result_ids.contains(&c.id) {
                        hidden.insert(c.id.clone());
                        continue;
                    }
                    // The raw provider proposal is retained in the SDK history. Native
                    // argument augmentation belongs to execution, not to the proposal.
                    calls.push(json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments.to_string()}}));
                }
            }
            if !calls.is_empty() {
                result.push(json!({"role":"assistant","content":null,"tool_calls":calls}));
            }
        } else if let Message::ToolResult(r) = message {
            if hidden.contains(&r.id) {
                if data_result_ids.contains(&r.id) {
                    result.push(json!({"role":"user","content":format!("Результат продолжения чтения источника (данные): {}",r.content)}));
                }
                continue;
            }
            result.push(json!({"role":"tool","tool_call_id":r.id,"content":r.content}));
        }
    }
    result
}
impl ModelProvider for Host {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("les", "host-transport", "assigned-model")
    }
    fn send_turn<'a>(&'a self, req: ModelRequest<'a>) -> ProviderFuture<'a> {
        Box::pin(async move {
            let continuation = {
                let mut p = self.policy.lock().unwrap();
                p.check().map_err(fault)?;
                if let Some(accepted) = &p.accepted {
                    return Ok(ModelResponse::Assistant(vec![ContentBlock::Text(
                        accepted["text"].as_str().unwrap_or("Completed").into(),
                    )]));
                }
                let coverage = p.coverage().map_err(fault)?;
                if coverage["complete"] != true && p.closed {
                    return Err(fault(
                        coverage["code"].as_str().unwrap_or("PDF_PAGE_INCOMPLETE"),
                    ));
                }
                if let Some(call) = coverage.get("continuation").filter(|v| v.is_object()) {
                    if !p.allowed.contains("read_pdf_source") {
                        return Err(fault("PDF_PAGE_INCOMPLETE"));
                    }
                    let id = p.private_id("cursor");
                    p.data_result_ids.insert(id.clone());
                    p.calls.insert(id.clone(), call.clone());
                    let round = p.rounds.len() + 1;
                    p.rounds.push(json!({"round":round,"proposed":1,"executed":0,"model_sent":false,"continuation":"native_pdf_cursor"}));
                    Some(ToolCall {
                        id,
                        name: "read_pdf_source".into(),
                        arguments: call["args"].clone(),
                    })
                } else {
                    None
                }
            };
            if let Some(call) = continuation {
                return Ok(ModelResponse::Assistant(vec![ContentBlock::ToolCall(call)]));
            }
            let final_answer = {
                let mut p = self.policy.lock().unwrap();
                p.model_messages = model_history(req.messages, &p.data_result_ids);
                p.closed
            };
            let decision = tokio::select! {d=self.ask(Step::Model{final_answer})=>d.map_err(fault)?,_=req.cancellation.cancelled()=>return Err(fault("CHAT_CANCELLED"))};
            match decision {
                Decision::Retry => Ok(self.control("continue", Value::Null)),
                Decision::Abort(code) => Err(fault(code)),
                Decision::Model(response) => {
                    let (calls, original) = {
                        let mut p = self.policy.lock().unwrap();
                        let calls = p.parse(&response, final_answer).map_err(fault)?;
                        p.batch += 1;
                        let original = response["tool_calls"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let round = p.rounds.len() + 1;
                        p.rounds.push(json!({"round":round,"proposed":calls.len(),"executed":0,"model_sent":true}));
                        (calls, original)
                    };
                    if calls.is_empty() {
                        let mut p = self.policy.lock().unwrap();
                        p.closed = true;
                        p.reason = "model_stop".into();
                        if response["text"]
                            .as_str()
                            .is_none_or(|s| s.trim().is_empty())
                        {
                            p.empty_answers += 1;
                            if final_answer || p.empty_answers > 1 {
                                return Err(fault("MODEL_EMPTY_RESPONSE"));
                            }
                            drop(p);
                            return Ok(self.control("continue", Value::Null));
                        }
                        drop(p);
                        return Ok(self.control("finalize", response));
                    }
                    let mut output = Vec::new();
                    let mut p = self.policy.lock().unwrap();
                    for (i, c) in calls.into_iter().enumerate() {
                        let id = c["call_id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| p.private_id("selection"));
                        if c["call_id"].is_null() {
                            p.data_result_ids.insert(id.clone());
                        }
                        let raw_args = original
                            .get(i)
                            .map(|c| &c["function"]["arguments"])
                            .cloned();
                        let args = raw_args
                            .map(|v| {
                                if let Some(s) = v.as_str() {
                                    serde_json::from_str(s).unwrap_or(Value::Null)
                                } else {
                                    v
                                }
                            })
                            .unwrap_or_else(|| c["args"].clone());
                        output.push(ContentBlock::ToolCall(ToolCall {
                            id: id.clone(),
                            name: c["tool"].as_str().unwrap().into(),
                            arguments: args,
                        }));
                        p.calls.insert(id, c);
                    }
                    Ok(ModelResponse::Assistant(output))
                }
                _ => Err(fault("AGENT_MODEL_NOT_REQUESTED")),
            }
        })
    }
}
#[derive(Debug)]
pub(super) struct Capability {
    pub host: Host,
    pub name: String,
}
impl Tool for Capability {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description:
                "LES host capability; actual provider schema is governed by its bound profile"
                    .into(),
            input_schema: json!({"type":"object"}),
        }
    }
    fn call<'a>(&'a self, inv: ToolInvocation, ctx: ToolContext) -> ToolFuture<'a> {
        Box::pin(async move {
            let perform = async {
                if self.name == CONTROL {
                    if inv.arguments()["kind"] == "continue" {
                        return Ok(ToolOutput::text("continue"));
                    }
                    let response = inv.arguments()["value"].clone();
                    match self.host.ask(Step::Complete(response)).await? {
                        Decision::Accept(value) => {
                            self.host.policy.lock().unwrap().accepted = Some(value);
                            Ok(ToolOutput::text("validated"))
                        }
                        Decision::Retry => Ok(ToolOutput::text("revise within existing budget")),
                        Decision::Abort(code) => Err(code),
                        _ => Err("AGENT_COMPLETION_REVISION_INVALID".into()),
                    }
                } else {
                    let (call, key) = {
                        let mut p = self.host.policy.lock().unwrap();
                        p.check()?;
                        let call = p
                            .calls
                            .get(inv.id().as_str())
                            .cloned()
                            .ok_or("TOOL_RESULT_MISMATCH")?;
                        let key = p.identity(&call);
                        // Idempotence is a product admission rule, never another scheduler.
                        if let Some(batch) = p.completed.get(&key) {
                            if *batch != p.batch {
                                p.closed = true;
                                p.reason = if p.failed_calls.contains(&key) {
                                    "repeated_tool_failure"
                                } else {
                                    "repeated_read"
                                }
                                .into();
                            }
                            return Ok(ToolOutput::text(
                                "Call already performed in this revision; no effect was repeated.",
                            ));
                        }
                        if p.remaining == 0 || p.closed {
                            p.closed = true;
                            p.reason = "calls_budget".into();
                            return Ok(ToolOutput::text(
                                "Tool budget closed; no action performed.",
                            ));
                        }
                        p.remaining -= 1;
                        if p.remaining == 0 {
                            p.closed = true;
                            p.reason = "calls_budget".into();
                        }
                        if let Some(r) = p.rounds.last_mut() {
                            r["executed"] = json!(r["executed"].as_u64().unwrap_or(0) + 1);
                        }
                        (call, key)
                    };
                    let name = call["tool"].as_str().unwrap().to_string();
                    match self.host.ask(Step::Tool(call)).await? {
                        Decision::Tool {
                            payload,
                            projection,
                        } => {
                            if payload["tool"] != name {
                                return Err("TOOL_RESULT_MISMATCH".into());
                            }
                            let failed = matches!(
                                payload["status"].as_str(),
                                Some("error" | "missing" | "blocked")
                            ) || payload["ok"] == false;
                            let mut p = self.host.policy.lock().unwrap();
                            if let Some(code) = &p.failed {
                                return Err(code.clone());
                            }
                            // Keep admission identities out of public results.
                            if failed {
                                p.failed_calls.insert(key.clone());
                            }
                            let batch = p.batch;
                            p.completed.insert(key, batch);
                            Ok(ToolOutput::text(projection.to_string()))
                        }
                        Decision::Abort(code) => Err(code),
                        _ => Err("AGENT_TOOL_NOT_REQUESTED".into()),
                    }
                }
            };
            let result = tokio::select! {r=perform=>r,_=ctx.cancellation().cancelled()=>Err("CHAT_CANCELLED".into())};
            result.map_err(|code: String| {
                self.host.abort(&code);
                ToolError::new(ToolErrorKind::Execution, code)
            })
        })
    }
}
