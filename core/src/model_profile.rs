//! Capacity bounds are shared by the native model runtime and compatibility clients.
use serde_json::{json, Value};

fn positive(value: &Value) -> Option<u64> {
    let value = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .or_else(|| value.as_bool().map(u64::from))
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
        .or_else(|| {
            value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 1. && *v < u64::MAX as f64)
                .map(|v| v as u64)
        });
    value.filter(|v| *v > 0)
}
fn narrow(preset: &mut Value, values: &Value) {
    let mut updates = serde_json::Map::new();
    for (alias, key) in [
        ("input_tokens", "input_token_limit"),
        ("input_token_limit", "input_token_limit"),
        ("normal_tool_count", "normal_tool_count"),
        ("max_tools", "max_tools"),
        ("max_batch_items", "max_batch_items"),
        ("parallel_read_limit", "parallel_read_limit"),
    ] {
        if let Some(value) = positive(&values[alias]) {
            updates.insert(key.into(), json!(value.min(preset[key].as_u64().unwrap())));
        }
    }
    let max = updates
        .get("max_tools")
        .unwrap_or(&preset["max_tools"])
        .as_u64()
        .unwrap();
    let normal = updates
        .get("normal_tool_count")
        .unwrap_or(&preset["normal_tool_count"])
        .as_u64()
        .unwrap()
        .min(max);
    updates.insert("normal_tool_count".into(), json!(normal));
    preset.as_object_mut().unwrap().extend(updates);
}
pub fn resolve(
    capacity: &Value,
    operator: &Value,
    restrictions: &Value,
) -> Result<Value, &'static str> {
    if !capacity.is_object()
        || !(operator.is_null() || operator.is_object())
        || !(restrictions.is_null() || restrictions.is_object())
    {
        return Err("MODEL_PROFILE_INVALID");
    }
    let context = capacity["observed"]
        .as_bool()
        .unwrap_or(false)
        .then(|| positive(&capacity["context_tokens"]))
        .flatten();
    let identity = capacity["model_id"]
        .as_str()
        .unwrap_or("")
        .to_lowercase()
        .split_whitespace()
        .collect::<String>();
    let extended = context.is_some_and(|n| n >= 16384)
        && identity.contains("qwen")
        && identity.contains("35b");
    let factory: Value = serde_json::from_str(include_str!("../../config/execution-presets.json"))
        .map_err(|_| "MODEL_PROFILE_FACTORY_INVALID")?;
    let mut preset = factory[if extended { "extended" } else { "restrictive" }].clone();
    let mut sources = vec![json!("workflow_invariants")];
    let requested = positive(
        operator
            .get("input_tokens")
            .unwrap_or(&operator["input_token_limit"]),
    );
    if let Some(context) = context {
        sources.push(json!("observed_backend_capacity"));
        let usable = if extended {
            context
        } else {
            context.min(32768)
        };
        preset["input_token_limit"] = json!(requested.map_or(usable, |n| usable.min(n)));
        if preset["input_token_limit"].as_u64().unwrap() >= 16384 {
            preset["generation_reserve_tokens"] =
                json!(4096.max(preset["generation_reserve_tokens"].as_u64().unwrap()));
        }
    } else if let Some(requested) = requested {
        preset["input_token_limit"] = json!(requested);
    }
    sources.push(json!("factory_preset"));
    for (values, source) in [
        (operator, "operator_clone"),
        (restrictions, "workflow_profile_restrictions"),
    ] {
        if values.as_object().is_some_and(|v| !v.is_empty()) {
            narrow(&mut preset, values);
            sources.push(json!(source));
        }
    }
    preset["source_chain"] = json!(sources);
    Ok(preset)
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    if request["schema"] != "les.model-profile.v1" {
        return Err("MODEL_PROFILE_SCHEMA_INVALID");
    }
    Ok(
        json!({"preset":resolve(&request["capacity"],&request["operator"],&request["restrictions"])?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_never_invents_capacity() {
        let capacity = json!({"model_id":"Qwen 35B","context_tokens":131072,"observed":false});
        let preset = resolve(&capacity, &Value::Null, &Value::Null).unwrap();
        assert_eq!(preset["input_token_limit"], 6000);
        assert_eq!(preset["max_tools"], 5);
        let mut observed = capacity;
        observed["observed"] = json!(true);
        let preset = resolve(&observed, &Value::Null, &Value::Null).unwrap();
        assert_eq!(preset["input_token_limit"], 131072);
        assert_eq!(preset["max_tools"], 8);
    }
    #[test]
    fn restrictions_only_narrow_and_keep_a_coherent_shortlist() {
        let result = resolve(
            &json!({"model_id":"Qwen3.5-35B","context_tokens":65536,"observed":true}),
            &json!({"input_tokens":32768,"max_tools":4}),
            &json!({"max_tools":2,"normal_tool_count":9}),
        )
        .unwrap();
        assert_eq!(result["input_token_limit"], 32768);
        assert_eq!(result["max_tools"], 2);
        assert_eq!(result["normal_tool_count"], 2);
    }

    #[test]
    fn generation_reserve_uses_the_requested_context_window() {
        let preset = resolve(
            &json!({"model_id":"Qwen3.5:2b","context_tokens":262144,"observed":true}),
            &json!({"input_tokens":8192}),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(preset["input_token_limit"], 8192);
        assert_eq!(preset["generation_reserve_tokens"], 1200);
    }
}
