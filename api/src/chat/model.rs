//! Consume the native provider stream; a broken stream is never a completed turn.
use super::{journal::Journal, prepare::Prepared, Result, Sink};
use crate::Context;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Instant;
pub async fn complete(
    context: &std::sync::Arc<Context>,
    turn: &Prepared,
    mut messages: Value,
    tools: Vec<Value>,
    max_output: u64,
    journal: &Journal,
    model_call: usize,
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
    let streamable = native || turn.connection.require(&["streaming".into()]).is_ok();
    sink.model_call(model_call);
    let started = Instant::now();
    let body = turn
        .connection
        .chat(
            &request,
            streamable,
            super::budget::remaining(deadline)?,
            context.listen_port,
            Some((journal.history,model_call)),
        )
        .await?;
    let mut stream = body.into_data_stream();
    let mut pending = Vec::new();
    let mut text = String::new();
    let mut finish = None;
    let mut metrics = Value::Null;
    let mut first_event_ms = None;
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.map_err(|_| "UPSTREAM_STREAM_INTERRUPTED")?;
        if pending.len() + bytes.len() > 4 * 1024 * 1024 {
            return Err(local_limit("normalized_buffer_bytes",pending.len()+bytes.len()));
        }
        pending.extend_from_slice(&bytes);
        while let Some(end) = pending.iter().position(|c| *c == b'\n') {
            let line: Vec<_> = pending.drain(..=end).collect();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let event: Value =
                serde_json::from_slice(&line).map_err(|_| "UPSTREAM_RESPONSE_INVALID")?;
            first_event_ms.get_or_insert_with(|| started.elapsed().as_millis());
            if let Some(value) = event.get("stream_metrics") {
                metrics = value.clone();
                metrics["first_decoded_event_ms"] = json!(first_event_ms);
                metrics["elapsed_ms"] = json!(started.elapsed().as_millis());
            }
            if let Some(code) = event["error"].as_str() {
                let mut error = super::error::Failure::new(code);
                error.stream_metrics = metrics.is_object().then_some(metrics);
                return Err(error);
            }
            if let Some(part) = event["text"].as_str() {
                text.push_str(part);
                if text.len() > 4 * 1024 * 1024 {
                    return Err(local_limit("visible_answer_bytes",text.len()));
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
    if metrics.is_object() {
        eprintln!("{}",json!({"event":"chat_provider_finished","history_id":journal.history,"model_call":model_call,"stream_metrics":metrics,"finish_reason":response["finish_reason"]}));
        response["stream_metrics"] = metrics;
    }
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

fn local_limit(name: &str, observed: usize) -> super::error::Failure {
    let mut error = super::error::Failure::new("UPSTREAM_RESPONSE_TOO_LARGE");
    error.stream_metrics = Some(
        json!({"protocol":"normalized_chat","limit":{"name":name,"observed":observed,"maximum":4*1024*1024}}),
    );
    error
}
