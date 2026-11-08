//! Bounded OpenAI chat decoder. Provider framing, reasoning and tool deltas stay native.
use crate::ollama_chat::{Limits, Reasoning};
use axum::body::{Body, Bytes};
use futures_util::{stream, StreamExt};
use les_core::chat_stream;
use serde_json::{json, Value};
use std::{collections::VecDeque, convert::Infallible};

pub const PROTOCOL: &str = "openai-chat.v1";
pub const WIRE: &str = "x-les-chat-wire";
const INVALID: &str = "UPSTREAM_STREAM_EVENT_INVALID";
const LIMIT: &str = "UPSTREAM_RESPONSE_TOO_LARGE";
const INTERRUPTED: &str = "UPSTREAM_STREAM_INTERRUPTED";

struct Decoder {
    limits: Limits,
    reasoning: Reasoning,
    collector: chat_stream::Stream,
    pending: VecDeque<Value>,
    data: Vec<u8>,
    lines: usize,
    consumed: usize,
    thinking: bool,
    finished: bool,
    saw_payload: bool,
    model: String,
}
impl Decoder {
    fn new(limits: Limits) -> Self {
        let thinking = limits.reasoning;
        let mut pending = VecDeque::new();
        if thinking {
            pending.push_back(json!({"thinking":true}));
        }
        Self {
            reasoning: Reasoning::new(thinking),
            limits,
            collector: chat_stream::Stream::default(),
            pending,
            data: Vec::new(),
            lines: 0,
            consumed: 0,
            thinking,
            finished: false,
            saw_payload: false,
            model: String::new(),
        }
    }
    fn event(&mut self, value: Value) -> Result<(), &'static str> {
        self.collector.push(value, 0)?;
        Ok(())
    }
    fn text(&mut self, value: String) -> Result<(), &'static str> {
        if !value.is_empty() {
            self.event(json!({"kind":"text_delta","text":value,"model_id":self.model}))?;
            self.pending.push_back(json!({"text":value}));
        }
        Ok(())
    }
    fn usage(&mut self, payload: &Value) -> Result<(), &'static str> {
        if let Some(usage) = payload["usage"].as_object() {
            let usage = usage
                .iter()
                .filter_map(|(key, v)| {
                    let integer = v.as_i64().or_else(|| v.as_str()?.parse().ok()).or_else(|| {
                        v.as_f64()
                            .filter(|v| v.is_finite() && *v >= 0. && *v < i64::MAX as f64)
                            .map(|v| v as i64)
                    });
                    integer.filter(|v| *v >= 0).map(|n| (key.clone(), json!(n)))
                })
                .collect::<serde_json::Map<_, _>>();
            self.event(json!({"kind":"usage","usage":usage}))?;
        }
        Ok(())
    }
    fn charge(&mut self, size: usize) -> Result<(), &'static str> {
        self.consumed = self.consumed.saturating_add(size);
        if self.consumed > self.limits.bytes {
            Err(LIMIT)
        } else {
            Ok(())
        }
    }
    fn finish(&mut self, reason: &str) -> Result<(), &'static str> {
        let tail = self.reasoning.finish(reason)?;
        self.text(tail)?;
        self.event(json!({"kind":"finish","finish_reason":reason,"model_id":self.model}))?;
        self.finished = true;
        Ok(())
    }
    fn terminal(&mut self) -> Result<(), &'static str> {
        if !self.saw_payload {
            return Err(INVALID);
        }
        if !self.finished {
            self.finish("")?;
        }
        let result = self.collector.push(Value::Null, 0)?;
        self.pending.push_back(json!({"finish":result}));
        Ok(())
    }
    fn payload(&mut self, raw: &[u8]) -> Result<bool, &'static str> {
        if raw == b"[DONE]" {
            self.terminal()?;
            return Ok(true);
        }
        let payload: Value = serde_json::from_slice(raw).map_err(|_| INVALID)?;
        if !payload.is_object() {
            return Err(INVALID);
        }
        if let Some(model) = payload["model"].as_str().filter(|v| !v.is_empty()) {
            self.model = model.into();
        }
        self.usage(&payload)?;
        if payload["usage"].is_object() && payload["choices"].as_array().is_none_or(Vec::is_empty) {
            return Ok(false);
        }
        let choice = payload["choices"]
            .as_array()
            .and_then(|v| v.first())
            .filter(|v| v.is_object())
            .ok_or(INVALID)?;
        let empty = json!({});
        let delta = choice
            .get("delta")
            .filter(|v| !v.is_null())
            .unwrap_or(&empty);
        if !delta.is_object() {
            return Err(INVALID);
        }
        self.saw_payload = true;
        // Usage-only frames after finish remain valid; extra text/tools do not.
        let thought = delta
            .get("reasoning")
            .filter(|v| !v.is_null() && **v != "")
            .or_else(|| delta.get("reasoning_content"))
            .unwrap_or(&Value::Null);
        let text = delta
            .get("content")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().ok_or(INVALID))
            .transpose()?
            .unwrap_or("");
        let tools = delta.get("tool_calls").filter(|v| !v.is_null());
        let extra =
            !text.is_empty() || tools.is_some_and(|v| v.as_array().is_none_or(|v| !v.is_empty()));
        if self.finished && extra {
            return Err(INVALID);
        }
        if !thought.is_null() && thought != "" {
            self.charge(
                thought
                    .as_str()
                    .map_or_else(|| thought.to_string().len(), str::len),
            )?;
            self.reasoning.separate();
            if !self.thinking {
                self.pending.push_back(json!({"thinking":true}));
                self.thinking = true;
            }
        }
        self.charge(text.len())?;
        let emitted = self.reasoning.feed(text);
        self.text(emitted)?;
        if let Some(tools) = tools {
            let calls = tools.as_array().ok_or("UPSTREAM_TOOL_CALLS_INVALID")?;
            self.charge(tools.to_string().len())?;
            self.event(json!({"kind":"tool_delta","tool_calls":calls.iter().filter(|v|v.is_object()).collect::<Vec<_>>(),"model_id":self.model}))?;
        }
        if let Some(reason) = choice["finish_reason"].as_str().filter(|v| !v.is_empty()) {
            if !self.finished {
                self.finish(reason)?;
            }
        }
        Ok(false)
    }
    fn line(&mut self, raw: &[u8]) -> Result<bool, &'static str> {
        self.lines = self.lines.saturating_add(1);
        if self.lines > self.limits.events || raw.len() > self.limits.line {
            return Err(LIMIT);
        }
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.is_empty() {
            if self.data.is_empty() {
                return Ok(false);
            }
            let data = std::mem::take(&mut self.data);
            return self.payload(&data);
        }
        if let Some(raw) = raw.strip_prefix(b"data:") {
            let raw = raw.strip_prefix(b" ").unwrap_or(raw);
            if self.data.len() + raw.len() + 1 > self.limits.line {
                return Err(LIMIT);
            }
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(raw);
        }
        Ok(false)
    }
    fn eof(&mut self) -> Result<(), &'static str> {
        // A semantic finish may precede a usage frame and transport EOF.
        if !self.data.is_empty() {
            return Err(INTERRUPTED);
        }
        if !self.finished {
            return Err(INTERRUPTED);
        }
        self.terminal()
    }
    fn complete(&mut self, raw: &[u8]) -> Result<(), &'static str> {
        let payload: Value =
            serde_json::from_slice(raw).map_err(|_| "UPSTREAM_RESPONSE_INVALID")?;
        let choice = payload["choices"]
            .as_array()
            .and_then(|v| v.first())
            .ok_or("UPSTREAM_RESPONSE_INVALID")?;
        let message = choice["message"]
            .as_object()
            .ok_or("UPSTREAM_RESPONSE_INVALID")?;
        self.model = payload["model"].as_str().unwrap_or("").into();
        self.saw_payload = true;
        self.usage(&payload)?;
        if message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning"))
            .is_some_and(|v| !v.is_null() && *v != "")
        {
            self.reasoning.separate();
        }
        let text = message
            .get("content")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().ok_or("UPSTREAM_RESPONSE_INVALID"))
            .transpose()?
            .unwrap_or("");
        let emitted = self.reasoning.feed(text);
        self.text(emitted)?;
        let calls = match message.get("tool_calls").filter(|v| !v.is_null()) {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .ok_or("UPSTREAM_TOOL_CALLS_INVALID")?
                .iter()
                .filter(|v| v.is_object())
                .cloned()
                .collect(),
        };
        let reason = choice["finish_reason"].as_str().unwrap_or("");
        let tail = self.reasoning.finish(reason)?;
        self.text(tail)?;
        self.event(json!({"kind":"finish","finish_reason":reason,"model_id":self.model,"tool_calls":calls}))?;
        self.finished = true;
        self.terminal()
    }
}

struct State {
    response: Option<crate::model_http::Source>,
    chunk: Bytes,
    line: Vec<u8>,
    decoder: Decoder,
    streaming: bool,
}
impl State {
    async fn next(&mut self) -> Result<bool, &'static str> {
        if !self.streaming {
            let mut data = Vec::new();
            while let Some(part) = self.response.as_mut().unwrap().next().await {
                let part = part?;
                if data.len() + part.len() > self.decoder.limits.bytes {
                    return Err(LIMIT);
                }
                data.extend_from_slice(&part);
            }
            self.decoder.complete(&data)?;
            return Ok(true);
        }
        loop {
            if self.chunk.is_empty() {
                match self.response.as_mut().unwrap().next().await {
                    Some(Ok(chunk)) => self.chunk = chunk,
                    Some(Err(code)) => return Err(code),
                    None => {
                        if !self.line.is_empty() {
                            return Err(INTERRUPTED);
                        }
                        self.decoder.eof()?;
                        return Ok(true);
                    }
                }
            }
            let newline = self.chunk.iter().position(|b| *b == b'\n');
            let size = newline.unwrap_or(self.chunk.len());
            if self.line.len() + size > self.decoder.limits.line {
                return Err(LIMIT);
            }
            self.line.extend_from_slice(&self.chunk.split_to(size));
            if newline.is_some() {
                let _ = self.chunk.split_to(1);
                let line = std::mem::take(&mut self.line);
                return self.decoder.line(&line);
            }
        }
    }
}
pub fn body(response: reqwest::Response, limits: Limits, streaming: bool) -> Body {
    from_source(crate::model_http::source(response), limits, streaming)
}
pub fn from_source(source: crate::model_http::Source, limits: Limits, streaming: bool) -> Body {
    let state = State {
        response: Some(source),
        chunk: Bytes::new(),
        line: Vec::new(),
        decoder: Decoder::new(limits),
        streaming,
    };
    Body::from_stream(stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.decoder.pending.pop_front() {
                let mut bytes = event.to_string().into_bytes();
                bytes.push(b'\n');
                return Some((Ok::<_, Infallible>(bytes), state));
            }
            state.response.as_ref()?;
            match state.next().await {
                Ok(false) => continue,
                Ok(true) => {}
                Err(code) => {
                    // Buffered data from an invalid frame is never emitted after its failure.
                    state.decoder.pending.clear();
                    state.decoder.pending.push_back(json!({"error":code}));
                    eprintln!("{}", json!({"event":"model_stream_failed","code":code}));
                }
            }
            state.response = None;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decoder() -> Decoder {
        Decoder::new(Limits::parse("2048/32768/0").unwrap())
    }
    #[test]
    fn split_tools_usage_and_unicode_are_preserved() {
        let mut d = decoder();
        for p in [
            json!({"model":"test","choices":[{"delta":{"content":"Лес 日本語 😀","tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_","arguments":"{\"page\":"}}]},"finish_reason":null}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"pdf_source","arguments":"6}"}}]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"completion_tokens":17}}),
        ] {
            d.payload(p.to_string().as_bytes()).unwrap();
        }
        assert!(d.payload(b"[DONE]").unwrap());
        let result = d.pending.back().unwrap()["finish"].clone();
        assert_eq!(result["text"], "Лес 日本語 😀");
        assert_eq!(
            result["tool_calls"][0]["function"]["name"],
            "read_pdf_source"
        );
        assert_eq!(
            result["tool_calls"][0]["function"]["arguments"],
            "{\"page\":6}"
        );
        assert_eq!(result["usage"]["completion_tokens"], 17);
    }
    #[test]
    fn interruption_is_not_a_successful_answer() {
        let mut d = decoder();
        d.payload(br#"{"choices":[{"delta":{"content":"partial"}}]}"#)
            .unwrap();
        assert_eq!(d.eof(), Err(INTERRUPTED));
        assert!(d.pending.iter().all(|v| v.get("finish").is_none()));
    }
    #[test]
    fn multiline_sse_and_budget_are_bounded() {
        let mut d = decoder();
        d.line(b":comment").unwrap();
        d.line(b"data: {\"choices\":").unwrap();
        d.line(b"data: [{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}")
            .unwrap();
        d.line(b"").unwrap();
        d.eof().unwrap();
        assert_eq!(d.pending.back().unwrap()["finish"]["text"], "ok");
        let mut d = Decoder::new(Limits::parse("2/5/0").unwrap());
        assert_eq!(
            d.payload(br#"{"choices":[{"delta":{"content":"123456"}}]}"#),
            Err(LIMIT)
        );
    }
}
