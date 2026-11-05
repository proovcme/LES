//! Request-local tool-loop control, independent of model choice and tool authorization.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::OnceLock, time::Instant};
type Result<T> = std::result::Result<T, &'static str>;
fn now() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}
fn key(v: &Value) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(v.as_str().ok_or("HARNESS_REQUEST_INVALID")?.as_bytes())
    ))
}
fn halt(s: &mut Value, reason: &str) {
    s["stopped"] = json!(true);
    s["reason"] = json!(reason);
}
fn eligible(s: &mut Value, at: f64) -> Result<bool> {
    if s["stopped"] == true {
        return Ok(false);
    }
    if s["remaining"].as_u64().ok_or("HARNESS_STATE_INVALID")? == 0 {
        halt(s, "calls_budget");
        return Ok(false);
    }
    if at >= s["deadline"].as_f64().ok_or("HARNESS_STATE_INVALID")? {
        halt(s, "deadline");
        return Ok(false);
    }
    Ok(true)
}
fn contains(s: &Value, field: &str, id: &str) -> bool {
    s[field]
        .as_array()
        .is_some_and(|items| items.iter().any(|v| v == id))
}
fn execute_at(r: &Value, at: f64) -> Result<Value> {
    let action = r["action"].as_str().ok_or("HARNESS_REQUEST_INVALID")?;
    if action == "new" {
        let limit = r["max_calls"].as_u64().ok_or("HARNESS_REQUEST_INVALID")?;
        let seconds = r["deadline_seconds"]
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.)
            .ok_or("HARNESS_REQUEST_INVALID")?;
        return Ok(
            json!({"state":{"schema":"les.tool-loop.v1","remaining":limit,"round":0,"deadline":at+seconds,"completed":[],"failed":[],"inflight":null,"stopped":false,"reason":"deadline"}}),
        );
    }
    let mut s = r["state"].clone();
    if s["schema"] != "les.tool-loop.v1" || !s["completed"].is_array() || !s["failed"].is_array() {
        return Err("HARNESS_STATE_INVALID");
    }
    let mut indexes = Vec::new();
    let allowed = match action {
        "round" => {
            if !s["inflight"].is_null() {
                return Err("HARNESS_INFLIGHT");
            }
            let allowed = eligible(&mut s, at)?;
            if allowed {
                s["round"] = json!(s["round"].as_u64().ok_or("HARNESS_STATE_INVALID")? + 1);
            }
            allowed
        }
        "select" => {
            let ids = r["identities"]
                .as_array()
                .ok_or("HARNESS_REQUEST_INVALID")?;
            if ids.is_empty() {
                halt(&mut s, "model_stop");
            } else if eligible(&mut s, at)? {
                let count = s["remaining"].as_u64().ok_or("HARNESS_STATE_INVALID")? as usize;
                for (index, id) in ids.iter().take(count).enumerate() {
                    let id = key(id)?;
                    if !contains(&s, "failed", &id) && !contains(&s, "completed", &id) {
                        indexes.push(index);
                    }
                }
                if indexes.is_empty() {
                    let successful = ids
                        .iter()
                        .map(key)
                        .collect::<Result<Vec<_>>>()?
                        .iter()
                        .any(|id| contains(&s, "completed", id) && !contains(&s, "failed", id));
                    halt(
                        &mut s,
                        if successful {
                            "repeated_read"
                        } else {
                            "repeated_tool_failure"
                        },
                    );
                }
            }
            !indexes.is_empty()
        }
        "begin" => {
            if !s["inflight"].is_null() {
                return Err("HARNESS_INFLIGHT");
            }
            let id = key(&r["identity"])?;
            if !eligible(&mut s, at)?
                || contains(&s, "completed", &id)
                || contains(&s, "failed", &id)
            {
                false
            } else {
                s["inflight"] = json!(id);
                true
            }
        }
        "finish" => {
            let id = s["inflight"]
                .as_str()
                .ok_or("HARNESS_NO_INFLIGHT")?
                .to_owned();
            let remaining = s["remaining"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or("HARNESS_STATE_INVALID")?;
            s["completed"].as_array_mut().unwrap().push(json!(id));
            if r["failed"] == true {
                s["failed"].as_array_mut().unwrap().push(json!(id));
            }
            s["inflight"] = Value::Null;
            s["remaining"] = json!(remaining - 1);
            eligible(&mut s, at)?;
            true
        }
        _ => return Err("HARNESS_REQUEST_INVALID"),
    };
    Ok(json!({"state":s,"allowed":allowed,"indexes":indexes}))
}
pub fn execute(r: &Value) -> Result<Value> {
    if matches!(
        r["action"].as_str(),
        Some("ledger_receipt" | "local_file_receipt" | "receipt_summary" | "no_local_changes")
    ) {
        return crate::receipts::execute(r);
    }
    execute_at(r, now())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(limit: u64) -> Value {
        execute_at(
            &json!({"action":"new","max_calls":limit,"deadline_seconds":10.}),
            0.,
        )
        .unwrap()["state"]
            .clone()
    }
    #[test]
    fn expired_selector_cannot_start_tools_and_inflight_result_is_kept() {
        let s = state(2);
        let late = execute_at(
            &json!({"action":"select","state":s,"identities":["write"]}),
            11.,
        )
        .unwrap();
        assert_eq!(late["state"]["reason"], "deadline");
        assert_eq!(late["indexes"], json!([]));
        let started =
            execute_at(&json!({"action":"begin","state":s,"identity":"read"}), 9.).unwrap();
        let finished = execute_at(
            &json!({"action":"finish","state":started["state"],"failed":false}),
            12.,
        )
        .unwrap();
        assert_eq!(finished["state"]["remaining"], 1);
        assert_eq!(finished["state"]["reason"], "deadline");
        assert_eq!(finished["state"]["completed"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn same_batch_duplicate_is_blocked_and_new_document_revision_is_allowed() {
        let s = execute_at(
            &json!({"action":"begin","state":state(3),"identity":"doc:1:read"}),
            1.,
        )
        .unwrap()["state"]
            .clone();
        let s = execute_at(&json!({"action":"finish","state":s,"failed":false}), 2.).unwrap()
            ["state"]
            .clone();
        assert_eq!(
            execute_at(
                &json!({"action":"begin","state":s,"identity":"doc:1:read"}),
                3.
            )
            .unwrap()["allowed"],
            false
        );
        assert_eq!(
            execute_at(
                &json!({"action":"begin","state":s,"identity":"doc:2:read"}),
                3.
            )
            .unwrap()["allowed"],
            true
        );
        assert_eq!(
            execute_at(
                &json!({"action":"select","state":s,"identities":["doc:1:read"]}),
                3.
            )
            .unwrap()["state"]["reason"],
            "repeated_read"
        );
    }
    #[test]
    fn failure_budget_and_terminal_text_have_distinct_stop_reasons() {
        let s = execute_at(
            &json!({"action":"begin","state":state(2),"identity":"bad"}),
            1.,
        )
        .unwrap()["state"]
            .clone();
        let s = execute_at(&json!({"action":"finish","state":s,"failed":true}), 2.).unwrap()
            ["state"]
            .clone();
        assert_eq!(
            execute_at(
                &json!({"action":"select","state":s,"identities":["bad"]}),
                3.
            )
            .unwrap()["state"]["reason"],
            "repeated_tool_failure"
        );
        assert_eq!(
            execute_at(
                &json!({"action":"select","state":state(2),"identities":[]}),
                11.
            )
            .unwrap()["state"]["reason"],
            "model_stop"
        );
        let s = execute_at(
            &json!({"action":"begin","state":state(1),"identity":"one"}),
            1.,
        )
        .unwrap()["state"]
            .clone();
        let s = execute_at(&json!({"action":"finish","state":s,"failed":false}), 2.).unwrap()
            ["state"]
            .clone();
        assert_eq!(s["reason"], "calls_budget");
        assert!(execute_at(&json!({"action":"finish","state":s}), 3.).is_err());
    }
}
