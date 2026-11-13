//! Whole-object context preparation and auditable source visibility. No model IO.
mod assembly;
mod coverage;
mod presentation;
mod prose;
mod tabular;
use serde_json::{json, Value};
use std::sync::OnceLock;
type Result<T> = std::result::Result<T, &'static str>;
const ERROR: &str = "CHAT_CONTEXT_INVALID";
const COVERAGE_ID: &str = "request:indexed-section-coverage";
pub fn table_rows(text: &str) -> Vec<(u64, Vec<String>)> {
    tabular::rows(text)
}
pub fn table_headers(text: &str) -> Vec<(u64, Vec<String>)> {
    tabular::headers(text)
}
pub fn prose_spans(text: &str, body_offset: usize) -> Vec<(u64, String)> {
    prose::spans(text, body_offset)
}
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
        .unwrap_or_else(|| evidence_render(&value["payload"]))
}
fn evidence_text(value: &Value, text: &str) -> String {
    let index = value["source_index"].as_u64().unwrap_or(0);
    let original = value["text"].as_str().unwrap_or("");
    let spans = value["body_offset"]
        .as_u64()
        .map(|offset| prose_spans(original, offset as usize))
        .unwrap_or_default();
    let rendered = tabular::render_with_prose(text, index, &spans);
    let body = value["body_offset"]
        .as_u64()
        .and_then(|n| value["text"].as_str()?.get(n as usize..));
    if body.is_some_and(|s| tabular::rows(s).is_empty()) {
        format!("[Фрагмент S{index}]\n{rendered}")
    } else {
        rendered
    }
}
fn evidence_render(value: &Value) -> String {
    if value["schema"] == "les.evidence-unit.v1" {
        let blocks = list(value, "blocks");
        if let Some(first) = blocks
            .first()
            .filter(|b| b["document_identity"].is_object())
        {
            if blocks
                .iter()
                .all(|b| b["document_identity"] == first["document_identity"])
            {
                return format!(
                    "{}\n{}",
                    render(&first["document_identity"]),
                    blocks
                        .iter()
                        .map(|b| {
                            let text = b["text"].as_str().unwrap_or("");
                            let compact = b["compact_header"].as_str().and_then(|h| {
                                text.split_once('\n')
                                    .map(|(_, tail)| format!("{h}\n{tail}"))
                            });
                            evidence_text(b, compact.as_deref().unwrap_or(text))
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n")
                );
            }
        }
        return blocks
            .iter()
            .map(evidence_render)
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    if value["schema"] == "les.evidence-block.v1" {
        if value["document_identity"].is_object() {
            return format!(
                "{}\n{}",
                render(&value["document_identity"]),
                evidence_text(value, value["text"].as_str().unwrap_or(""))
            );
        }
        return evidence_text(value, value["text"].as_str().unwrap_or(""));
    }
    render(value)
}
// Prefer the complete reading unit. Assembly supplies explicit, whole-block
// fallbacks for a smaller round window; coverage still reports missing pages.
fn evidence_objects(prefix: &str, values: &[Value]) -> Vec<Value> {
    let mut groups: Vec<Value> = Vec::new();
    let mut positions = std::collections::HashMap::new();
    for value in values {
        if let Some(key) = value["context_unit"]
            .as_str()
            .filter(|k| !k.is_empty())
            .filter(|_| value["schema"] == "les.evidence-block.v1")
        {
            let pos = *positions.entry(key.to_owned()).or_insert_with(|| {
                groups.push(json!({"schema":"les.evidence-unit.v1","blocks":[]}));
                groups.len() - 1
            });
            groups[pos]["blocks"]
                .as_array_mut()
                .unwrap()
                .push(value.clone());
        } else {
            groups.push(value.clone());
        }
    }
    objects(prefix, &groups)
}
fn tokens(text: &str) -> i64 {
    ((text.chars().count() + 1) / 2).max(1) as i64
}
fn object(id: &str, payload: Value) -> Value {
    json!({"object_id":id,"render":evidence_render(&payload),"payload":payload})
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
                .flat_map(|v| evidence_indexes(&v["payload"], &object_render(v)))
        })
        .collect();
    sources
        .iter()
        .filter(|s| !s.is_object() || !truth(&s["index"]) || indexes.contains(&s["index"]))
        .cloned()
        .collect()
}
fn evidence_indexes(value: &Value, rendered: &str) -> Vec<Value> {
    if value["schema"] == "les.evidence-unit.v1" {
        return list(value, "blocks")
            .iter()
            .flat_map(|v| evidence_indexes(v, &evidence_render(v)))
            .collect();
    }
    if value["schema"] == "les.evidence-block.v1" {
        return value["source_index"]
            .as_u64()
            .filter(|n| *n > 0)
            .map(|n| vec![json!(n)])
            .unwrap_or_default();
    }
    // A legacy single-source block uses its first header. Tags occurring in
    // document prose are data and cannot admit a different, omitted source.
    labels(rendered.lines().next().unwrap_or(""))
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
