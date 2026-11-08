//! Fresh, observed capabilities gate execution; template declarations never grant access.
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub fn require(snapshot: &Value, required: &[String], now: Option<&str>) -> Result<(), String> {
    let expiry = snapshot["expires_at"]
        .as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .ok_or("CAPABILITY_SNAPSHOT_INVALID")?;
    let now = match now {
        Some(value) => chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|_| "CAPABILITY_CLOCK_INVALID")?
            .with_timezone(&chrono::Utc),
        None => chrono::Utc::now(),
    };
    if now >= expiry {
        return Err("CAPABILITY_SNAPSHOT_STALE".into());
    }
    let observations = snapshot["observations"]
        .as_array()
        .ok_or("CAPABILITY_SNAPSHOT_INVALID")?;
    for capability in required.iter().collect::<BTreeSet<_>>() {
        if !matches!(
            capability.as_str(),
            "chat_completions"
                | "streaming"
                | "tools"
                | "structured_output"
                | "responses"
                | "embeddings"
                | "models"
                | "token_count"
                | "rerank"
        ) {
            return Err("CAPABILITY_NAME_INVALID".into());
        }
        let observation = observations.iter().find(|v| v["capability"] == *capability);
        if observation.is_none_or(|v| v["state"] != "supported") {
            return Err(format!("CAPABILITY_REQUIRED: {capability}"));
        }
        let observation = observation.unwrap();
        if matches!(
            observation["evidence_source"].as_str(),
            Some("template_default" | "unavailable")
        ) {
            return Err(format!("CAPABILITY_EVIDENCE_INSUFFICIENT: {capability}"));
        }
        if !matches!(
            observation["evidence_source"].as_str(),
            Some("probe" | "operator_declaration")
        ) {
            return Err("CAPABILITY_SNAPSHOT_INVALID".into());
        }
    }
    Ok(())
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    if request["schema"] != "les.model-capabilities.v1" {
        return Err("CAPABILITY_SCHEMA_INVALID");
    }
    let required = request["required"]
        .as_array()
        .ok_or("CAPABILITY_REQUEST_INVALID")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("CAPABILITY_REQUEST_INVALID")
        })
        .collect::<Result<Vec<_>, _>>()?;
    match require(&request["snapshot"], &required, request["now"].as_str()) {
        Ok(()) => Ok(json!({"status":"allowed"})),
        Err(code) => Ok(json!({"status":"blocked","code":code})),
    }
}
