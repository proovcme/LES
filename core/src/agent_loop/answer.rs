//! Bounded final generation. Only an empty answer can request a stricter second attempt.
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub(super) fn run(
    request: &Value,
    bridge: impl Fn(u8, &Value) -> Result<Value, &'static str>,
) -> Result<Value, &'static str> {
    let attempts = request["max_attempts"]
        .as_u64()
        .filter(|v| (1..=2).contains(v))
        .ok_or("AGENT_REQUEST_INVALID")?;
    let seconds = request["deadline_seconds"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0. && *v <= 3600.)
        .ok_or("AGENT_REQUEST_INVALID")?;
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    for attempt in 1..=attempts {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(json!({"status":"failed","code":"ANSWER_DEADLINE_EXCEEDED"}));
        }
        let result = bridge(
            0,
            &json!({"attempt":attempt,"strict":attempt>1,
            "timeout_ms":remaining.as_millis().min(u64::MAX as u128) as u64}),
        )?;
        if let Some(error) = result.get("error") {
            return Ok(
                json!({"status":"failed","code":error["code"].as_str().unwrap_or("CHAT_IO_FAILED")}),
            );
        }
        let response = &result["response"];
        if !response.is_object() || !response["text"].is_string() {
            return Err("UPSTREAM_RESPONSE_INVALID");
        }
        if response.get("tool_calls").is_some_and(|value| {
            !value.is_null() && value.as_array().is_none_or(|calls| !calls.is_empty())
        }) {
            return Ok(json!({"status":"failed","code":"TOOL_BUDGET_EXHAUSTED"}));
        }
        if !response["text"].as_str().unwrap().trim().is_empty() {
            return Ok(json!({"status":"completed","attempts":attempt,"terminal":response}));
        }
    }
    Ok(json!({"status":"failed","code":"MODEL_EMPTY_RESPONSE","attempts":attempts}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn empty_answer_gets_one_strict_retry_and_never_repeats_a_valid_answer() {
        let calls = Cell::new(0);
        let result = run(
            &json!({"max_attempts":2,"deadline_seconds":10}),
            |_, request| {
                calls.set(calls.get() + 1);
                assert_eq!(request["strict"], calls.get() == 2);
                Ok(json!({"response":{"text":if calls.get()==1 {""} else {"Answer"}}}))
            },
        )
        .unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(result["attempts"], 2);
        assert_eq!(result["terminal"]["text"], "Answer");
    }
    #[test]
    fn final_tool_request_or_transport_failure_never_retries() {
        for response in [
            json!({"error":{"code":"MODEL_UNAVAILABLE"}}),
            json!({"response":{"text":"I did it","tool_calls":[{}]}}),
        ] {
            let calls = Cell::new(0);
            let result = run(&json!({"max_attempts":2,"deadline_seconds":10}), |_, _| {
                calls.set(calls.get() + 1);
                Ok(response.clone())
            })
            .unwrap();
            assert_eq!(result["status"], "failed");
            assert_eq!(calls.get(), 1);
        }
    }
}
