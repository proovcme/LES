//! Bound tools remain reachable with their original permission and effect checks.
use super::{envelope, Context, Prepared, Result, Tools};
use serde_json::{json, Value};
use std::sync::Arc;
pub(super) fn contract(tools: &[Value]) -> Value {
    let mut names: Vec<_> = tools
        .iter()
        .filter(|v| eligible(v))
        .filter_map(|v| v["name"].as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let name = format!(
        "use_extensions_{}",
        &les_core::sha256_hex(names.join("\n").as_bytes())[..12]
    );
    json!({"name":name,"version":"1.0.0","title":"Инструменты выбранного профиля","category":"extensions",
        "summary":"Access the profile's approved read/compute tools and its bound local workspace/table tools. List, describe a name for its exact input schema, then call with operation=call, name and arguments. Every call retains the target tool's scope and permission checks. Source folders are read-only; external writes, arbitrary code and commands are unavailable.",
        "input_schema":{"type":"object","properties":{"operation":{"type":"string","enum":["list","describe","call"]},"name":{"type":"string"},"arguments":{"type":"object"},"offset":{"type":"integer","minimum":0}},"required":["operation"],"additionalProperties":false},
        "result_schema":"les_tool_result_v1","effect":"compute","scopes":["extensions"],"timeout_seconds":1200,"retry":"never","idempotency":"none",
        "result_budget":{"max_chars":60000,"max_items":4096},"model_owned_fields":[],"approval_required":false,"provenance":"Frozen selected profile","tags":["extensions"]})
}
fn eligible(c: &Value) -> bool {
    ["read", "compute", "local_state"]
        .iter()
        .any(|s| c["effect"] == *s)
}
pub(super) async fn run(
    tools: &Tools,
    context: Arc<Context>,
    turn: &Prepared,
    name: &str,
    args: Value,
    run_id: &str,
    deadline: std::time::Instant,
) -> Result<Value> {
    let eligible: Vec<_> = tools
        .contracts
        .iter()
        .filter(|c| c["name"] != name && eligible(c))
        .collect();
    let operation = args["operation"].as_str().ok_or("TOOL_SCHEMA_INVALID")?;
    let result = if operation == "list" {
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let page:Vec<_>=eligible.iter().skip(offset).take(5).map(|c|json!({"name":c["name"],"title":c["title"],"summary":c["summary"].as_str().unwrap_or("").chars().take(300).collect::<String>()})).collect();
        json!({"next_offset":if offset+page.len()<eligible.len(){Some(offset+page.len())}else{None},"tools":page})
    } else {
        let target = args["name"].as_str().ok_or("TOOL_NOT_ALLOWED")?;
        let contract = eligible
            .into_iter()
            .find(|c| c["name"] == target)
            .ok_or("TOOL_NOT_ALLOWED")?;
        if operation == "describe" {
            let mut description = contract.clone();
            description["call_via"] =
                json!({"name":name,"arguments":{"operation":"call","name":target,"arguments":{}}});
            description["call_instruction"] = json!("Call the function named in call_via.name. Preserve its outer operation=call and name; fill only its nested arguments using input_schema. Do not put the target operation (start/read/record) or target fields at the outer level.");
            description
        } else if operation == "call" {
            Box::pin(tools.run(
                context,
                turn,
                &json!({"tool":target,"args":args.get("arguments").cloned().unwrap_or(json!({}))}),
                run_id,
                deadline,
            ))
            .await?
        } else {
            return Err("TOOL_SCHEMA_INVALID".into());
        }
    };
    let failed = operation == "call" && result["status"] != "ok";
    let mut payload = envelope(name, operation, result, vec![]);
    if failed {
        payload["status"] = json!("error");
    }
    Ok(payload)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_commit_and_destructive_effects_are_never_extension_calls() {
        for effect in ["external", "commit", "destructive", "draft"] {
            assert!(!eligible(&json!({"effect":effect})));
        }
        for effect in ["read", "compute", "local_state"] {
            assert!(eligible(&json!({"effect":effect})));
        }
        let ext = contract(&[json!({"name":"dataset_map","effect":"read"})]);
        assert_eq!(ext["result_budget"]["max_chars"], 60000);
        assert_eq!(ext["result_budget"]["max_items"], 4096);
    }
}
