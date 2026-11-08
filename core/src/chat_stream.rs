//! Assemble normalized chat events without buffering provider reasoning.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

const INVALID: &str = "UPSTREAM_STREAM_EVENT_INVALID";
const INVALID_CALLS: &str = "UPSTREAM_TOOL_CALLS_INVALID";
const TOO_LARGE: &str = "UPSTREAM_RESPONSE_TOO_LARGE";

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
pub struct Stream {
    text: String,
    calls: BTreeMap<u64, ToolCall>,
    final_calls: Option<Vec<Value>>,
    usage: Map<String, Value>,
    model: String,
    reason: String,
    finished: bool,
    bytes: usize,
}

impl Stream {
    pub fn push(&mut self, event: Value, bytes: usize) -> Result<Value, &'static str> {
        if event.is_null() {
            if !self.finished {
                return Err("UPSTREAM_STREAM_INTERRUPTED");
            }
            let calls = self.final_calls.take().unwrap_or_else(|| {
                std::mem::take(&mut self.calls)
                    .into_values()
                    .map(|call| {
                        json!({"id":call.id, "type":"function",
                           "function":{"name":call.name, "arguments":call.arguments}})
                    })
                    .collect()
            });
            return Ok(
                json!({"text":std::mem::take(&mut self.text), "tool_calls":calls,
                "usage":std::mem::take(&mut self.usage), "model_id":self.model,
                "finish_reason":self.reason}),
            );
        }
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > 32 * 1024 * 1024 {
            return Err(TOO_LARGE);
        }
        if let Some(model) = nonempty(&event["model_id"]) {
            model.clone_into(&mut self.model);
        }
        match event["kind"].as_str() {
            Some("text_delta") => self.text.push_str(event["text"].as_str().ok_or(INVALID)?),
            Some("reasoning") => {}
            Some("usage") => {
                self.usage
                    .extend(event["usage"].as_object().ok_or(INVALID)?.clone());
            }
            Some("tool_delta") => {
                for delta in event["tool_calls"].as_array().ok_or(INVALID_CALLS)? {
                    self.tool_delta(delta)?;
                }
            }
            Some("finish") => {
                self.finished = true;
                if let Some(reason) = nonempty(&event["finish_reason"]) {
                    reason.clone_into(&mut self.reason);
                }
                if let Some(calls) = event["tool_calls"]
                    .as_array()
                    .filter(|calls| !calls.is_empty())
                {
                    self.final_calls = Some(calls.clone());
                }
            }
            _ => return Err(INVALID),
        }
        Ok(json!({}))
    }

    fn tool_delta(&mut self, delta: &Value) -> Result<(), &'static str> {
        let index = match delta.get("index") {
            Some(value) => value.as_u64().filter(|&n| n < 256).ok_or(INVALID_CALLS)?,
            None => 0,
        };
        let call = self.calls.entry(index).or_default();
        if let Some(id) = nonempty(&delta["id"]) {
            id.clone_into(&mut call.id);
        }
        let function = &delta["function"];
        if !function.is_null() && !function.is_object() {
            return Err(INVALID_CALLS);
        }
        if let Some(name) = function.get("name") {
            call.name.push_str(name.as_str().ok_or(INVALID_CALLS)?);
        }
        if let Some(args) = function.get("arguments") {
            call.arguments.push_str(args.as_str().ok_or(INVALID_CALLS)?);
        }
        Ok(())
    }
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}
