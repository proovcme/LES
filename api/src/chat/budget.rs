//! Read admission uses the same packer as the subsequent model call.
use super::Result;
use serde_json::{json, Value};
use std::{
    future::Future,
    time::{Duration, Instant},
};

/// One absolute deadline belongs to the request, including preparation and IO.
pub(super) fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| "CHAT_DEADLINE_EXCEEDED".into())
}

pub(super) async fn until<T>(
    deadline: Instant,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    remaining(deadline)?;
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), work)
        .await
        .map_err(|_| "CHAT_DEADLINE_EXCEEDED")?
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[tokio::test]
    async fn expired_request_does_not_start_another_action() {
        let mut invoked = false;
        let result = until(Instant::now(), async {
            invoked = true;
            Ok(())
        })
        .await;
        assert_eq!(result.unwrap_err().code, "CHAT_DEADLINE_EXCEEDED");
        assert!(!invoked);
    }
    #[tokio::test]
    async fn an_inflight_operation_and_the_next_round_share_the_deadline() {
        let deadline = Instant::now() + Duration::from_millis(30);
        let error = until(deadline, std::future::pending::<Result<()>>())
            .await
            .unwrap_err();
        assert_eq!(error.code, "CHAT_DEADLINE_EXCEEDED");
        assert!(remaining(deadline).is_err());
        assert!(until(deadline, async { Ok(()) }).await.is_err());
    }
}

pub(super) fn reader_capacity(
    request: &Value,
    proof_blocks: usize,
    exchange_groups: usize,
) -> Result<usize> {
    let mut baseline = request.clone();
    for field in [
        "evidence",
        "source_map",
        "checkpoint",
        "working_memory",
        "tool_exchange",
        "native_tool_history",
    ] {
        baseline[field] = json!([]);
    }
    baseline["required_evidence"] = json!(request["required_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .skip(proof_blocks)
        .collect::<Vec<_>>());
    baseline["native_tool_exchange"] = json!(request["native_tool_exchange"]
        .as_array()
        .into_iter()
        .flatten()
        .skip(exchange_groups)
        .collect::<Vec<_>>());
    let dialogue = request["dialogue"].as_array().cloned().unwrap_or_default();
    let last = dialogue
        .iter()
        .rposition(|m| m["role"] == "user")
        .unwrap_or(0);
    baseline["dialogue"] = json!(&dialogue[last..]);
    let packed = les_core::chat_context::execute(&baseline)?;
    if packed["overflow"] == true {
        return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
    }
    let packet = &packed["packet"];
    let free = packet["input_budget_tokens"]
        .as_u64()
        .ok_or("CHAT_CONTEXT_INVALID")?
        .saturating_sub(
            packet["included_tokens"]
                .as_u64()
                .ok_or("CHAT_CONTEXT_INVALID")?,
        );
    // Core estimates two characters/token. Keep 256 tokens for the new call
    // envelope, coverage and the grant notice; readers reserve identity per block.
    Ok(free.saturating_sub(256).saturating_mul(2).clamp(1, 32000) as usize)
}
