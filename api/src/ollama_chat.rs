//! Bounded Ollama NDJSON decoder for the private model bridge.
use axum::body::{Body, Bytes};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{collections::VecDeque, convert::Infallible};

pub const PROTOCOL: &str = "ollama-chat.v1";
pub const LIMITS_HEADER: &str = "x-les-chat-limits";
pub const PROTOCOL_HEADER: &str = "x-les-model-protocol";

const INVALID: &str = "UPSTREAM_RESPONSE_INVALID";
const TOO_LARGE: &str = "UPSTREAM_RESPONSE_TOO_LARGE";
const INTERRUPTED: &str = "UPSTREAM_STREAM_INTERRUPTED";
const REASONING_EXHAUSTED: &str = "REASONING_BUDGET_EXHAUSTED";

pub struct Limits {
    pub(crate) bytes: usize,
    pub(crate) events: usize,
    pub(crate) line: usize,
    pub(crate) reasoning: bool,
}

impl Limits {
    pub fn parse(header: &str) -> Option<Self> {
        let mut parts = header.split('/');
        let tokens: usize = parts.next()?.parse().ok()?;
        let bytes: usize = parts.next()?.parse().ok()?;
        let reasoning = match parts.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        if parts.next().is_some()
            || !(1..=1_048_576).contains(&bytes)
            || !(1..=1_048_576).contains(&tokens)
        {
            return None;
        }
        Some(Self {
            bytes,
            events: (tokens * 8).max(256),
            line: (bytes * 8).max(1024),
            reasoning,
        })
    }
}

#[derive(PartialEq, Eq)]
enum Mode {
    Pending,
    Thinking,
    Answer,
}

pub(crate) struct Reasoning {
    mode: Mode,
    buffer: String,
    observed: bool,
    answer_seen: bool,
}

impl Reasoning {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            mode: if enabled { Mode::Pending } else { Mode::Answer },
            buffer: String::new(),
            observed: false,
            answer_seen: false,
        }
    }

    pub(crate) fn separate(&mut self) {
        self.observed = true;
        if self.mode == Mode::Pending && self.buffer.is_empty() {
            self.mode = Mode::Answer;
        }
    }

    pub(crate) fn feed(&mut self, text: &str) -> String {
        if self.mode == Mode::Answer {
            self.answer_seen |= !text.trim().is_empty();
            return text.to_owned();
        }
        let offset = self.buffer.len().saturating_sub(7);
        self.buffer.push_str(text);
        // Delimiters are ASCII. Only the tail of the previous fragment can
        // start a delimiter newly completed by this fragment.
        let window = &self.buffer.as_bytes()[offset..];
        if let Some(close) = window.windows(8).position(|part| part == b"</think>") {
            let answer = self.buffer.split_off(offset + close + 8);
            self.buffer.clear();
            self.mode = Mode::Answer;
            self.observed = true;
            self.answer_seen = !answer.trim().is_empty();
            return answer;
        }
        if self.mode == Mode::Pending && window.windows(7).any(|part| part == b"<think>") {
            self.mode = Mode::Thinking;
            self.observed = true;
        }
        if self.mode == Mode::Thinking {
            let start = self
                .buffer
                .floor_char_boundary(self.buffer.len().saturating_sub(7));
            self.buffer.drain(..start);
        }
        String::new()
    }

    pub(crate) fn finish(&mut self, reason: &str) -> Result<String, &'static str> {
        let exhausted = matches!(reason, "length" | "max_tokens");
        let missing = self.mode == Mode::Pending || self.observed && !self.answer_seen;
        if self.mode == Mode::Thinking || exhausted && missing {
            return Err(REASONING_EXHAUSTED);
        }
        Ok(std::mem::take(&mut self.buffer))
    }
}

struct Chat {
    limits: Limits,
    reasoning: Reasoning,
    thinking_reported: bool,
    consumed: usize,
    events: usize,
    saw_message: bool,
    pending: VecDeque<Value>,
    tools: Vec<Value>,
    model: Option<String>,
    reason: String,
    prompt_tokens: i64,
    completion_tokens: i64,
}

impl Chat {
    fn new(limits: Limits) -> Self {
        let reasoning = Reasoning::new(limits.reasoning);
        let mut pending = VecDeque::new();
        if limits.reasoning {
            pending.push_back(json!({"thinking":true}));
        }
        Self {
            thinking_reported: limits.reasoning,
            limits,
            reasoning,
            consumed: 0,
            events: 0,
            saw_message: false,
            pending,
            tools: Vec::new(),
            model: None,
            reason: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
        }
    }

    fn charge(&mut self, size: usize) -> Result<(), &'static str> {
        self.consumed += size;
        if self.consumed > self.limits.bytes {
            return Err(TOO_LARGE);
        }
        Ok(())
    }

    fn text(&mut self, text: String) {
        if !text.is_empty() {
            self.pending.push_back(json!({"text":text}));
        }
    }

    fn line(&mut self, line: &[u8]) -> Result<bool, &'static str> {
        self.events += 1;
        if self.events > self.limits.events {
            return Err(TOO_LARGE);
        }
        let line = std::str::from_utf8(line).map_err(|_| INVALID)?;
        if line.trim().is_empty() {
            return Ok(false);
        }
        let mut frame: Value = serde_json::from_str(line).map_err(|_| INVALID)?;
        self.prompt_tokens = token_count(&frame["prompt_eval_count"], self.prompt_tokens)?;
        self.completion_tokens = token_count(&frame["eval_count"], self.completion_tokens)?;
        let message = frame
            .get_mut("message")
            .and_then(Value::as_object_mut)
            .ok_or(INVALID)?;
        let thought = message
            .get("thinking")
            .filter(|value| truthy(value))
            .or_else(|| {
                message
                    .get("reasoning_content")
                    .filter(|value| truthy(value))
            });
        if let Some(thought) = thought {
            self.charge(rendered_size(thought))?;
            self.reasoning.separate();
            if !self.thinking_reported {
                self.pending.push_back(json!({"thinking":true}));
                self.thinking_reported = true;
            }
        }
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            self.charge(text.len())?;
            let text = self.reasoning.feed(text);
            self.text(text);
        }
        if let Some(tools) = message.remove("tool_calls").filter(truthy) {
            let calls = tools.as_array().ok_or("UPSTREAM_TOOL_CALLS_INVALID")?;
            // Keep the legacy semantic budget including JSON separator spaces.
            self.charge(rendered_size(&tools))?;
            self.tools
                .extend(calls.iter().filter(|value| value.is_object()).cloned());
        }
        self.saw_message = true;
        if let Some(model) = frame["model"].as_str().filter(|text| !text.is_empty()) {
            self.model = Some(model.to_owned());
        }
        if let Some(reason) = frame["done_reason"]
            .as_str()
            .filter(|text| !text.is_empty())
        {
            reason.clone_into(&mut self.reason);
        }
        if frame["done"] != true {
            return Ok(false);
        }
        let tail = self.reasoning.finish(&self.reason)?;
        let total = self
            .prompt_tokens
            .checked_add(self.completion_tokens)
            .ok_or(INVALID)?;
        self.text(tail);
        self.pending.push_back(json!({"finish":{
            "model_id": self.model.take(), "finish_reason":self.reason,
            "tool_calls":std::mem::take(&mut self.tools),
            "usage":{"prompt_tokens":self.prompt_tokens,"completion_tokens":self.completion_tokens,
                     "total_tokens":total}
        }}));
        Ok(true)
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn rendered_size(value: &Value) -> usize {
    value.as_str().map_or_else(
        || value.to_string().len() + separator_spaces(value),
        str::len,
    )
}

fn separator_spaces(value: &Value) -> usize {
    match value {
        Value::Array(items) => {
            items.len().saturating_sub(1) + items.iter().map(separator_spaces).sum::<usize>()
        }
        Value::Object(items) => {
            items.len()
                + items.len().saturating_sub(1)
                + items.values().map(separator_spaces).sum::<usize>()
        }
        _ => 0,
    }
}

fn token_count(value: &Value, prior: i64) -> Result<i64, &'static str> {
    if !truthy(value) {
        return Ok(prior);
    }
    let count = match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            let value = number.as_f64()?;
            (number.is_f64() && (i64::MIN as f64..i64::MAX as f64).contains(&value))
                .then_some(value as i64)
        }),
        Value::String(value) => value.trim().parse().ok(),
        Value::Bool(true) => Some(1),
        _ => None,
    };
    count.ok_or(INVALID)
}

struct Stream {
    response: Option<crate::model_http::Source>,
    chunk: Bytes,
    line: Vec<u8>,
    chat: Chat,
}

impl Stream {
    async fn line(&mut self) -> Result<Option<Vec<u8>>, &'static str> {
        loop {
            if self.chunk.is_empty() {
                match self.response.as_mut().unwrap().next().await {
                    Some(Ok(chunk)) => self.chunk = chunk,
                    None => {
                        return Ok((!self.line.is_empty()).then(|| std::mem::take(&mut self.line)))
                    }
                    Some(Err(code)) => return Err(code),
                }
            }
            let newline = self.chunk.iter().position(|byte| *byte == b'\n');
            let count = newline.unwrap_or(self.chunk.len());
            if self.line.len() + count > self.chat.limits.line {
                return Err(TOO_LARGE);
            }
            self.line.extend_from_slice(&self.chunk.split_to(count));
            if newline.is_some() {
                let _ = self.chunk.split_to(1);
                return Ok(Some(std::mem::take(&mut self.line)));
            }
        }
    }
}

pub fn body(response: reqwest::Response, limits: Limits) -> Body {
    from_source(crate::model_http::source(response), limits)
}
pub fn from_source(source: crate::model_http::Source, limits: Limits) -> Body {
    let state = Stream {
        response: Some(source),
        chunk: Bytes::new(),
        line: Vec::new(),
        chat: Chat::new(limits),
    };
    Body::from_stream(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.chat.pending.pop_front() {
                let mut bytes = event.to_string().into_bytes();
                bytes.push(b'\n');
                return Some((Ok::<_, Infallible>(bytes), state));
            }
            state.response.as_ref()?;
            let result = match state.line().await {
                Ok(Some(line)) => state.chat.line(&line),
                Ok(None) => Err(if state.chat.saw_message {
                    INTERRUPTED
                } else {
                    INVALID
                }),
                Err(code) => Err(code),
            };
            match result {
                Ok(false) => continue,
                Ok(true) => {}
                Err(code) => {
                    eprintln!("{}", json!({"event":"model_stream_failed","code":code}));
                    state.chat.pending.push_back(json!({"error":code}));
                }
            }
            state.response = None;
        }
    }))
}
