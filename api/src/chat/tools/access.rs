//! Bounded on-demand schemas keep all profile-approved read/compute tools reachable.
use super::{envelope, Context, Prepared, Result, Tools};
use serde_json::{json, Value};
use std::sync::Arc;
pub(super) fn contract(tools: &[Value]) -> Value {
    let mut names: Vec<_> = tools
        .iter()
        .filter(|v| v["effect"] == "read" || v["effect"] == "compute")
        .filter_map(|v| v["name"].as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    let name = format!(
        "use_extensions_{}",
        &les_core::sha256_hex(names.join("\n").as_bytes())[..12]
    );
    json!({"name":name,"version":"1.0.0","title":"Инструменты выбранного профиля","category":"extensions",
        "summary":"Access the profile's approved read/compute tools. List, describe a name for its exact input schema, then call it with arguments. Cursor pagination uses next_offset. Source folders are read-only; arbitrary code and commands are unavailable.",
        "input_schema":{"type":"object","properties":{"operation":{"type":"string","enum":["list","describe","call"]},"name":{"type":"string"},"arguments":{"type":"object"},"offset":{"type":"integer","minimum":0}},"required":["operation"],"additionalProperties":false},
        "result_schema":"les_tool_result_v1","effect":"compute","scopes":["extensions"],"timeout_seconds":1200,"retry":"never","idempotency":"none",
        "result_budget":{"max_chars":60000,"max_items":32},"model_owned_fields":[],"approval_required":false,"provenance":"Frozen selected profile","tags":["extensions"]})
}
pub(super) async fn run(
    tools: &Tools,
    context: Arc<Context>,
    turn: &Prepared,
    name: &str,
    args: Value,
    run_id: &str,
) -> Result<Value> {
    let eligible: Vec<_> = tools
        .contracts
        .iter()
        .filter(|c| c["name"] != name && (c["effect"] == "read" || c["effect"] == "compute"))
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
            contract.clone()
        } else if operation == "call" {
            Box::pin(tools.run(
                context,
                turn,
                &json!({"tool":target,"args":args.get("arguments").cloned().unwrap_or(json!({}))}),
                run_id,
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
