//! Bounded non-interactive model replies share the same normalized stream boundary.
use axum::body::{to_bytes, Body};
use serde_json::Value;
pub async fn collect(body: Body, kind: &str) -> Result<String, String> {
    let bytes = to_bytes(body, 2 * 1024 * 1024)
        .await
        .map_err(|_| format!("{kind}_RESPONSE_LIMIT"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| format!("{kind}_MODEL_RESPONSE_INVALID"))?;
    let mut output = String::new();
    let mut finished = false;
    for line in text.lines().filter(|s| !s.trim().is_empty()) {
        if finished {
            return Err(format!("{kind}_MODEL_RESPONSE_INVALID"));
        }
        let event: Value =
            serde_json::from_str(line).map_err(|_| format!("{kind}_MODEL_RESPONSE_INVALID"))?;
        if let Some(code) = event["error"].as_str() {
            return Err(code.into());
        }
        if let Some(delta) = event["text"].as_str() {
            output.push_str(delta);
        }
        if let Some(finish) = event.get("finish") {
            if !matches!(finish["finish_reason"].as_str(), Some("stop" | "eos")) {
                return Err(format!("{kind}_MODEL_TRUNCATED"));
            }
            if finish["tool_calls"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
            {
                return Err(format!("{kind}_UNEXPECTED_TOOL"));
            }
            finished = true;
        }
    }
    if !finished || output.trim().is_empty() {
        return Err(format!("{kind}_MODEL_INTERRUPTED"));
    }
    Ok(output)
}
