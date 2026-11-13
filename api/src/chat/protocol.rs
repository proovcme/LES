//! Provider call/result pairing and bounded ledger projections.
use serde_json::{json, Value};
/// Keep provider call/result groups indivisible when the context window moves.
pub(super) fn exchange_groups(exchanges: &[Value]) -> Vec<Value> {
    let mut groups: Vec<Vec<Value>> = Vec::new();
    for message in exchanges {
        if message["role"] != "tool" || groups.is_empty() {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(message.clone());
    }
    groups.into_iter().map(Value::Array).collect()
}
pub(super) fn bound_pair(proposal: &Value, content: Value) -> Value {
    json!([{"role":"assistant","content":null,"tool_calls":[proposal]},
        {"role":"tool","tool_call_id":proposal["id"],"name":proposal["function"]["name"],"content":content.to_string()}])
}
pub(super) fn proposal(pending: &Value, call: &Value) -> Value {
    pending
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == call["call_id"] && p["function"]["name"] == call["tool"])
        .cloned()
        .unwrap_or(Value::Null)
}
/// A skipped proposal still needs a provider result; never replay its effect.
pub(super) fn complete_pending(exchanges: &mut Vec<Value>, reason: &str) {
    let Some(start) = exchanges
        .iter()
        .rposition(|v| v["role"] == "assistant" && v["tool_calls"].is_array())
    else {
        return;
    };
    let calls = exchanges[start]["tool_calls"].as_array().unwrap().clone();
    for call in calls {
        let Some(id) = call["id"].as_str() else {
            continue;
        };
        if exchanges[start + 1..]
            .iter()
            .any(|m| m["role"] == "tool" && m["tool_call_id"] == id)
        {
            continue;
        }
        let result = json!({"schema":"les_tool_result_v1","tool":call["function"]["name"],"status":"error","operation":"coordinator_guard","result":{"code":"TOOL_CALL_NOT_EXECUTED","reason":reason,"executed":false,"instruction":"This proposed call was skipped by the repetition or call-budget guard. No new action was performed. Read the current task state and prior results; choose the next different required action or report the remaining work."}});
        exchanges.push(json!({"role":"tool","tool_call_id":id,"content":result.to_string()}));
    }
}
pub(super) fn effective(mut payload: &Value) -> &Value {
    for _ in 0..8 {
        if payload["operation"] == "call"
            && payload["tool"]
                .as_str()
                .is_some_and(|s| s.starts_with("use_extensions_"))
            && payload["result"]["schema"] == "les_tool_result_v1"
        {
            payload = &payload["result"];
        } else {
            break;
        }
    }
    payload
}
fn is_bound(call: &Value, bound: &str) -> bool {
    if call["function"]["name"] == bound {
        return true;
    }
    let args = call["function"]["arguments"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| call["function"]["arguments"].clone());
    call["function"]["name"]
        .as_str()
        .is_some_and(|s| s.starts_with("use_extensions_"))
        && args["operation"] == "call"
        && args["name"] == bound
}
pub(super) fn strip_bound_calls(exchanges: &mut Vec<Value>, bound: &str) {
    let mut ids = Vec::new();
    let mut kept = Vec::new();
    for mut message in exchanges.drain(..) {
        if message["role"] == "assistant" {
            ids = message["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| is_bound(c, bound))
                .map(|c| c["id"].clone())
                .collect();
            if let Some(calls) = message["tool_calls"].as_array_mut() {
                calls.retain(|c| !is_bound(c, bound));
            }
            if message["tool_calls"]
                .as_array()
                .is_some_and(|c| c.is_empty())
                && message["content"].is_null()
            {
                continue;
            }
        } else if message["role"] == "tool" && ids.contains(&message["tool_call_id"]) {
            continue;
        }
        kept.push(message);
    }
    *exchanges = kept;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_groups_keep_reused_provider_ids_with_their_own_results() {
        let call = |name| json!({"role":"assistant","content":null,"tool_calls":[{"id":"same","function":{"name":name}}]});
        let result = |text| json!({"role":"tool","tool_call_id":"same","content":text});
        let before = vec![
            call("search_sources"),
            result("search receipt"),
            call("read_pdf_source"),
            result("page receipt"),
        ];
        assert_eq!(
            exchange_groups(&before),
            vec![json!([before[0], before[1]]), json!([before[2], before[3]])]
        );
    }
    #[test]
    fn server_owned_pdf_continuations_can_leave_the_window_without_fake_call_ids() {
        let before = vec![
            json!({"role":"assistant","tool_calls":[{"id":"provider"}]}),
            json!({"role":"tool","tool_call_id":"provider","content":"first page part"}),
            json!({"role":"user","content":"server continuation 1"}),
            json!({"role":"user","content":"server continuation 2"}),
        ];
        let groups = exchange_groups(&before);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], json!([before[0], before[1]]));
        assert_eq!(groups[2], json!([before[3]]));
        assert!(!groups[2].to_string().contains("tool_call_id"));
    }
    #[test]
    fn skipped_provider_call_gets_a_negative_result_without_replaying_prior_effects() {
        let call = json!({"role":"assistant","content":null,"tool_calls":[{"id":"same","function":{"name":"filesystem_stat","arguments":{}}}]});
        let mut messages = vec![
            call.clone(),
            json!({"role":"tool","tool_call_id":"same","content":"old response"}),
            call,
        ];
        complete_pending(&mut messages, "duplicate");
        assert_eq!(messages.len(), 4);
        let result: Value = serde_json::from_str(messages[3]["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["result"]["executed"], false);
        assert_eq!(result["result"]["code"], "TOOL_CALL_NOT_EXECUTED");
        complete_pending(&mut messages, "duplicate");
        assert_eq!(messages.len(), 4);
    }
    #[test]
    fn table_projection_preserves_other_groups_with_reused_provider_ids() {
        let call = |name: &str| json!({"role":"assistant","content":null,"tool_calls":[{"id":"same","function":{"name":name}}]});
        let result = |value: &str| json!({"role":"tool","tool_call_id":"same","content":value});
        let mut messages = vec![
            call("read_pdf_source"),
            result("PDF before"),
            call("table_document"),
            result("old packet"),
            call("read_source"),
            result("PDF after"),
        ];
        strip_bound_calls(&mut messages, "table_document");
        assert_eq!(
            messages,
            vec![
                call("read_pdf_source"),
                result("PDF before"),
                call("read_source"),
                result("PDF after")
            ]
        );
    }
}
