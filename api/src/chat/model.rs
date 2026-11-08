//! Consume the native provider stream; a broken stream is never a completed turn.
use super::{journal::Journal, prepare::Prepared, Result, Sink};
use crate::Context;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
pub async fn complete(
    context: &Context,
    turn: &Prepared,
    messages: Value,
    tools: Vec<Value>,
    journal: &Journal,
    sink: &Sink,
) -> Result<Value> {
    let profile = &turn.connection.profile;
    let maximum = profile["generation_reserve_tokens"]
        .as_u64()
        .unwrap_or(2048);
    let max_output = match turn.request["response_length"].as_str() {
        Some("short") => maximum.min(768),
        _ => maximum,
    };
    let temperature = les_core::chat_context::execute(
        &json!({"action":"temperature","profile":turn.profile,"fallback":0.1}),
    )?;
    let request = json!({"messages":messages,"tools":tools,"max_output_tokens":max_output,"reasoning_enabled":profile["reasoning_enabled"].as_bool().unwrap_or(false),
        "temperature":temperature,"response_format":null});
    let native = turn.connection.snapshot["transport_options"]["chat_protocol"] == "native_chat_v1";
    let streamable = native
        || les_core::model_capabilities::require(
            &turn.connection.snapshot,
            &["streaming".into()],
            None,
        )
        .is_ok();
    let upstream_port = reqwest::Url::parse(&context.upstream)
        .ok()
        .and_then(|u| u.port())
        .unwrap_or(0);
    let body = turn
        .connection
        .chat(
            &request,
            streamable,
            Duration::from_secs(600),
            [context.listen_port, upstream_port],
        )
        .await?;
    let mut stream = body.into_data_stream();
    let mut pending = Vec::new();
    let mut text = String::new();
    let mut finish = None;
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.map_err(|_| "UPSTREAM_STREAM_INTERRUPTED")?;
        if pending.len() + bytes.len() > 4 * 1024 * 1024 {
            return Err("UPSTREAM_RESPONSE_TOO_LARGE".into());
        }
        pending.extend_from_slice(&bytes);
        while let Some(end) = pending.iter().position(|c| *c == b'\n') {
            let line: Vec<_> = pending.drain(..=end).collect();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let event: Value =
                serde_json::from_slice(&line).map_err(|_| "UPSTREAM_RESPONSE_INVALID")?;
            if let Some(code) = event["error"].as_str() {
                return Err(code.into());
            }
            if let Some(part) = event["text"].as_str() {
                text.push_str(part);
                if text.len() > 4 * 1024 * 1024 {
                    return Err("UPSTREAM_RESPONSE_TOO_LARGE".into());
                }
                journal.checkpoint("token", json!(part)).await?;
                sink.emit("token", json!(part)).await?;
            }
            if event["thinking"] == true {
                sink.progress("generation", "Модель обдумывает ответ")
                    .await?;
            }
            if event["finish"].is_object() {
                if finish.replace(event["finish"].clone()).is_some() {
                    return Err("UPSTREAM_RESPONSE_INVALID".into());
                }
            }
        }
    }
    if !pending.iter().all(u8::is_ascii_whitespace) {
        return Err("UPSTREAM_STREAM_INTERRUPTED".into());
    }
    let mut response = finish.ok_or("UPSTREAM_STREAM_INTERRUPTED")?;
    response["text"] = json!(text);
    if response["tool_calls"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        journal.checkpoint("reset", json!("")).await?;
        sink.emit("reset", json!("")).await?;
    }
    Ok(response)
}
