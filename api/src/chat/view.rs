//! Project the authoritative turn into the public chat and history contract.
use serde_json::{json, Value};

pub(super) fn decorate(mut response: Value) -> Value {
    response["completion_status"] = response["artifact"]["completion_status"].clone();
    response["execution"] = response["artifact"]["execution_summary"].clone();
    let sources = response["source_map"].clone();
    let execution = response["execution"].clone();
    let trace = &mut response["retrieval_trace"];
    trace["source_map"] = sources;
    trace["execution"] = execution;
    if let Some(citations) = trace.get("citation_syntax") {
        let linked = &citations["indexes"];
        let unknown = &citations["unknown_indexes"];
        let status = if unknown.as_array().is_some_and(|rows| !rows.is_empty()) {
            "unresolved"
        } else if linked.as_array().is_some_and(|rows| !rows.is_empty()) {
            "linked"
        } else {
            "not_cited"
        };
        trace["citation_check"] = json!({"status":status,"linked":linked,"unresolved":unknown});
    }
    response
}
