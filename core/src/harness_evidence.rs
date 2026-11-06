//! Model tool calls and evidence admitted into the next model turn.
use regex::Regex;
use serde_json::{json, Value};
use std::{collections::HashSet, sync::OnceLock};

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn tool_calls(request: &Value) -> Result<Value, &'static str> {
    static FENCE: OnceLock<Regex> = OnceLock::new();
    let allowed = request["allowed"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let allowed: HashSet<_> = allowed.iter().filter_map(Value::as_str).collect();
    let limit = request["max_calls"]
        .as_u64()
        .ok_or("HARNESS_REQUEST_INVALID")?
        .max(1);
    let raw = text(&request["text"]).trim();
    let fence =
        FENCE.get_or_init(|| Regex::new(r"(?is)\x60{3}(?:json)?\s*(\{.*?\})\s*\x60{3}").unwrap());
    let raw = fence
        .captures(raw)
        .and_then(|found| found.get(1))
        .map_or(raw, |found| found.as_str());
    let Some((start, end)) = raw
        .find('{')
        .zip(raw.rfind('}'))
        .filter(|(start, end)| start < end)
    else {
        return Ok(json!({"calls":[]}));
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw[start..=end]) else {
        return Ok(json!({"calls":[]}));
    };
    let calls = &parsed["calls"];
    let items = if calls.is_object() {
        std::slice::from_ref(calls)
    } else {
        calls.as_array().map(Vec::as_slice).unwrap_or_default()
    };
    let mut result = Vec::new();
    for item in items {
        let tool = text(&item["tool"]);
        let tool = if tool.is_empty() {
            text(&item["name"])
        } else {
            tool
        }
        .trim();
        if !allowed.contains(tool) {
            continue;
        }
        let args = item["args"].as_object().cloned().unwrap_or_default();
        let mut call = json!({"tool":tool,"args":args});
        let id = [&item["call_id"], &item["id"]]
            .into_iter()
            .find_map(|value| match value {
                Value::String(id) if !id.is_empty() => Some(id.clone()),
                Value::Number(id) if id.as_f64() != Some(0.0) => Some(id.to_string()),
                _ => None,
            });
        if let Some(id) = id {
            call["call_id"] = json!(id);
        }
        result.push(call);
        if result.len() as u64 >= limit {
            break;
        }
    }
    Ok(json!({"calls":result}))
}

fn evidence(request: &Value) -> Result<Value, &'static str> {
    let existing = request["existing"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let proposed = request["proposed"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let exact = request["exact"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    let exact: HashSet<_> = exact
        .iter()
        .map(|key| (text(&key[0]), text(&key[1])))
        .collect();
    let mut known: HashSet<_> = existing.iter().map(text).collect();
    let mut selected = Vec::new();
    for (index, item) in proposed.iter().enumerate() {
        let id = item["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("HARNESS_REQUEST_INVALID")?;
        if !exact.contains(&(text(&item["dataset"]), text(&item["file"]))) && known.insert(id) {
            selected.push(index);
        }
    }
    Ok(json!({"indexes":selected}))
}

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    match request["action"].as_str() {
        Some("parse_tool_calls") => tool_calls(request),
        Some("merge_tool_evidence") => evidence(request),
        _ => Err("HARNESS_REQUEST_INVALID"),
    }
}
