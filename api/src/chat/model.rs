//! Consume the native provider stream; a broken stream is never a completed turn.
use super::{journal::Journal, prepare::Prepared, Result, Sink};
use crate::Context;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Instant;
fn output_limit(maximum: u64, short: bool, tools: &[Value], format: &Value) -> u64 {
    // Brevity controls user prose. Calls and structured selections need the
    // reserved protocol budget; truncating their JSON cannot make them shorter.
    if short && tools.is_empty() && format.is_null() {
        maximum.min(768)
    } else {
        maximum
    }
}
pub async fn complete(
    context: &std::sync::Arc<Context>,
    turn: &Prepared,
    mut messages: Value,
    tools: Vec<Value>,
    journal: &Journal,
    sink: &Sink,
    publish_text: bool,
    response_format: Value,
    deadline: Instant,
) -> Result<Value> {
    super::budget::until(deadline, async {
    if turn.egress_tag.is_some() {
        let owner = context.clone();
        let ids = turn.scope["resolved_dataset_ids"].clone();
        tokio::task::spawn_blocking(move || super::egress::check(&owner, &ids))
            .await
            .map_err(|_| "CLOUD_SCOPE_UNVERIFIED")??;
    }
    let profile = &turn.connection.profile;
    let maximum = profile["generation_reserve_tokens"]
        .as_u64()
        .unwrap_or(2048);
    let max_output = output_limit(maximum, turn.request["response_length"] == "short", &tools, &response_format);
    let temperature = les_core::chat_context::execute(
        &json!({"action":"temperature","profile":turn.profile,"fallback":0.1}),
    )?;
    if let Some(url) = turn.attachment["data_url"].as_str() {
        let message = messages
            .as_array_mut()
            .and_then(|rows| rows.iter_mut().rev().find(|m| m["role"] == "user"))
            .ok_or("INVALID_MODEL_MESSAGE")?;
        let text = message["content"]
            .as_str()
            .ok_or("INVALID_MODEL_MESSAGE")?
            .to_owned();
        message["content"] =
            json!([{"type":"text","text":text},{"type":"image_url","image_url":{"url":url}}]);
    }
    let request = json!({"messages":messages,"tools":tools,"max_output_tokens":max_output,"reasoning_enabled":profile["reasoning_enabled"].as_bool().unwrap_or(false),
        "temperature":temperature,"response_format":response_format});
    let native = turn.connection.snapshot["transport_options"]["chat_protocol"] == "native_chat_v1";
    let streamable = native
        || les_core::model_capabilities::require(
            &turn.connection.snapshot,
            &["streaming".into()],
            None,
        )
        .is_ok();
    let upstream_port = context.upstream_port().unwrap_or(0);
    let body = turn
        .connection
        .chat(
            &request,
            streamable,
            super::budget::remaining(deadline)?,
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
                if publish_text {
                    journal.checkpoint("token", json!(part)).await?;
                    sink.emit("token", json!(part)).await?;
                }
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
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_prose_does_not_truncate_calls_or_structured_selections() {
        assert_eq!(output_limit(1200, true, &[], &Value::Null), 768);
        assert_eq!(
            output_limit(1200, true, &[json!({"type":"function"})], &Value::Null),
            1200
        );
        assert_eq!(
            output_limit(1200, true, &[], &json!({"type":"json_schema"})),
            1200
        );
        assert_eq!(output_limit(1200, false, &[], &Value::Null), 1200);
        // The model's declared reserve is always authoritative, even below the
        // prose limit. This never grants additional tokens or changes its role.
        assert_eq!(output_limit(256, true, &[json!({})], &Value::Null), 256);
        assert_eq!(output_limit(256, true, &[], &Value::Null), 256);
    }
}
