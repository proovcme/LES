//! Job decisions are native; the existing SQLite store remains authoritative.
use serde_json::{json, Map, Value};

const ACTIVE: &[&str] = &["queued", "running", "parsing"];
const TERMINAL: &[&str] = &["completed", "failed", "cancelled"];

pub fn patch(request: &Value) -> Result<Value, &'static str> {
    let current = request["current"].as_object().ok_or("CORE_JOB_INVALID")?;
    let incoming = request["patch"]
        .as_object()
        .ok_or("CORE_JOB_PATCH_INVALID")?;
    let now = request["now"].as_str().ok_or("CORE_JOB_TIME_INVALID")?;
    let mut result = Map::new();
    for (key, value) in incoming {
        match key.as_str() {
            "total" | "processed" | "errors" => {
                if value.as_i64().is_none_or(|n| n < 0) {
                    return Err("CORE_JOB_COUNTER_INVALID");
                }
            }
            "status" => {
                let status = value
                    .as_str()
                    .ok_or("CORE_JOB_STATUS_INVALID")?
                    .to_lowercase();
                if !ACTIVE.contains(&status.as_str()) && !TERMINAL.contains(&status.as_str()) {
                    return Err("CORE_JOB_STATUS_INVALID");
                }
                result.insert(key.clone(), Value::String(status));
                continue;
            }
            "source" | "dataset_id" | "dataset_name" | "message" => {
                if !value.is_string() {
                    return Err("CORE_JOB_FIELD_INVALID");
                }
            }
            "result" => {
                let encoded = if let Some(text) = value.as_str() {
                    let _: Value =
                        serde_json::from_str(text).map_err(|_| "CORE_JOB_RESULT_INVALID")?;
                    text.to_owned()
                } else {
                    serde_json::to_string(value).map_err(|_| "CORE_JOB_RESULT_INVALID")?
                };
                result.insert(key.clone(), Value::String(encoded));
                continue;
            }
            _ => return Err("CORE_JOB_UPDATE_FIELD_INVALID"),
        }
        result.insert(key.clone(), value.clone());
    }
    let previous = current
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let finished = current
        .get("finished_at")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if (finished || TERMINAL.contains(&previous.as_str()))
        && (result
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| s != previous)
            || ["total", "processed", "errors"]
                .iter()
                .any(|key| result.contains_key(*key)))
    {
        return Ok(json!({"ignored":true,"patch":{}}));
    }
    result.insert("updated_at".into(), json!(now));
    if result
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| TERMINAL.contains(&s))
    {
        result.insert(
            "finished_at".into(),
            current
                .get("finished_at")
                .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                .cloned()
                .unwrap_or_else(|| json!(now)),
        );
    }
    Ok(json!({"ignored":false,"patch":result}))
}

pub fn recovery(request: &Value) -> Result<Value, &'static str> {
    let current = &request["current"];
    let status = current["status"].as_str().unwrap_or("").to_lowercase();
    let finished = current["finished_at"]
        .as_str()
        .is_some_and(|s| !s.is_empty());
    if finished || !ACTIVE.contains(&status.as_str()) {
        return Ok(json!({"ignored":true,"patch":{}}));
    }
    let reason = request["reason"]
        .as_str()
        .ok_or("CORE_JOB_REASON_INVALID")?;
    let before = current["message"].as_str().unwrap_or("");
    let message = if before.is_empty() {
        format!("Interrupted by {reason}")
    } else {
        format!("{before} | Interrupted by {reason}")
    };
    patch(
        &json!({"current":current,"patch":{"status":"cancelled","message":message},"now":request["now"]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_jobs_do_not_resurrect_and_unknown_fields_are_rejected() {
        let current = json!({"status":"completed","finished_at":"first"});
        let late = patch(
            &json!({"current":current,"patch":{"status":"running","processed":5},"now":"later"}),
        )
        .unwrap();
        assert_eq!(late["ignored"], true);
        assert!(patch(&json!({"current":{},"patch":{"status='failed' --":1},"now":"x"})).is_err());
    }
    #[test]
    fn recovery_keeps_progress_and_is_idempotent() {
        let row = json!({"status":"PARSING","processed":7,"message":"reading"});
        let recovered = recovery(&json!({"current":row,"reason":"restart","now":"now"})).unwrap();
        assert_eq!(recovered["patch"]["status"], "cancelled");
        assert_eq!(
            recovered["patch"]["message"],
            "reading | Interrupted by restart"
        );
        assert!(recovered["patch"].get("processed").is_none());
        assert_eq!(
            recovery(&json!({"current":{"status":"cancelled"},"reason":"restart","now":"now"}))
                .unwrap()["ignored"],
            true
        );
    }
}
