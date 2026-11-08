//! Whole-object context preparation and auditable source visibility. No model IO.
mod assembly;
mod coverage;
mod presentation;
use serde_json::{json, Value};
use std::sync::OnceLock;
type Result<T> = std::result::Result<T, &'static str>;
const ERROR: &str = "CHAT_CONTEXT_INVALID";
const COVERAGE_ID: &str = "request:indexed-section-coverage";
fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.),
    }
}
fn string(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        if value.is_null() {
            String::new()
        } else {
            value.to_string()
        }
    })
}
fn render(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    crate::tool_execution::canonical::encode(value).unwrap_or_else(|_| value.to_string())
}
fn object_render(value: &Value) -> String {
    value["render"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| render(&value["payload"]))
}
fn tokens(text: &str) -> i64 {
    ((text.chars().count() + 1) / 2).max(1) as i64
}
fn object(id: &str, payload: Value) -> Value {
    json!({"object_id":id,"render":render(&payload),"payload":payload})
}
fn objects(prefix: &str, values: &[Value]) -> Vec<Value> {
    values
        .iter()
        .enumerate()
        .filter(|(_, v)| {
            !v.is_null()
                && !matches!(v, Value::String(s) if s.is_empty())
                && !matches!(v, Value::Array(a) if a.is_empty())
                && !matches!(v, Value::Object(o) if o.is_empty())
        })
        .map(|(i, v)| object(&format!("{prefix}:{i}"), v.clone()))
        .collect()
}
fn labels(text: &str) -> Vec<Value> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?m)^\[Источник (\d+)(?:\s|\])").unwrap())
        .captures_iter(text)
        .filter_map(|m| m[1].parse::<u64>().ok().map(|v| json!(v)))
        .collect()
}
fn visible(packet: &Value, sources: &[Value]) -> Vec<Value> {
    let indexes: Vec<_> = list(packet, "sections")
        .iter()
        .filter(|s| s["kind"] == "evidence")
        .flat_map(|s| {
            list(s, "objects")
                .iter()
                .flat_map(|v| labels(&object_render(v)))
        })
        .collect();
    sources
        .iter()
        .filter(|s| !s.is_object() || !truth(&s["index"]) || indexes.contains(&s["index"]))
        .cloned()
        .collect()
}
pub fn execute(request: &Value) -> Result<Value> {
    match request["action"].as_str().ok_or(ERROR)? {
        "assemble" => assembly::assemble(request),
        "messages" => assembly::messages(&request["packet"], request["request_last"] == true),
        "objects" => Ok(json!(objects(
            request["prefix"].as_str().ok_or(ERROR)?,
            list(request, "values")
        ))),
        "text_objects" => Ok(json!(objects(
            request["prefix"].as_str().ok_or(ERROR)?,
            &string(&request["text"])
                .split("\n\n")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
                .collect::<Vec<_>>()
        ))),
        "workspace_memory" => Ok(json!(list(request, "candidates")
            .iter()
            .flat_map(|c| list(c, "objects").iter().filter(move |o| {
                let id = o["object_id"].as_str().unwrap_or("");
                (c["kind"] == "working_memory"
                    && ["agent-file:", "agent-memory-omitted:"]
                        .iter()
                        .any(|p| id.starts_with(p)))
                    || id == "agent-memory-unavailable"
                    || (request["registered"] == true
                        && ((c["kind"] == "working_memory" && id.starts_with("note:"))
                            || (c["kind"] == "checkpoint"
                                && id.starts_with("conversation-summary:"))))
            }))
            .cloned()
            .collect::<Vec<_>>())),
        "visible" => Ok(json!(visible(
            &request["packet"],
            list(request, "source_map")
        ))),
        "trace" | "prepared_trace" => presentation::trace(request),
        "confirm" => {
            let mut trace = request["trace"].clone();
            if !trace.is_object() {
                return Err(ERROR);
            }
            trace["delivery"] = json!("confirmed");
            trace["sent_to_model"] = json!(true);
            if let Some(n) = request["usage"]["prompt_tokens"]
                .as_u64()
                .filter(|n| *n > 0)
            {
                trace["provider_prompt_tokens"] = json!(n);
            }
            Ok(trace)
        }
        "section_coverage" => Ok(coverage::report(
            list(request, "sources"),
            list(request, "visible"),
            false,
        )),
        "model_coverage" => Ok(coverage::report(
            list(request, "sources"),
            list(request, "visible"),
            true,
        )),
        "coverage_reservation" => {
            Ok(coverage::reservation(list(request, "sources")).unwrap_or(Value::Null))
        }
        "finalize_coverage" => coverage::finalize(
            request["packet"].clone(),
            list(request, "sources"),
            list(request, "visible"),
        ),
        _ => presentation::execute(request),
    }
}
