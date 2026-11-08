//! Bind model-selected tools to explicit request scope without replacing professional decisions.
use serde_json::{json, Value};

fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value.as_f64() != Some(0.),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(super) fn augment(request: &Value) -> Result<Value, &'static str> {
    let original = &request["call"];
    let tool = original["tool"].as_str().ok_or("HARNESS_REQUEST_INVALID")?;
    let mut args = original["args"].as_object().cloned().unwrap_or_default();
    let datasets = request["dataset_ids"]
        .as_array()
        .ok_or("HARNESS_REQUEST_INVALID")?;
    if datasets.iter().any(|value| !value.is_string()) {
        return Err("HARNESS_REQUEST_INVALID");
    }
    let has = |args: &serde_json::Map<String, Value>, key: &str| args.get(key).is_some_and(present);
    if tool == "dataset_map" && !datasets.is_empty() && !has(&args, "dataset_id") {
        args.insert("dataset_id".into(), datasets[0].clone());
    }
    if matches!(
        tool,
        "search_sources"
            | "read_source"
            | "read_pdf_source"
            | "read_excel_source"
            | "look_at_pdf_page"
    ) {
        if !matches!(tool, "search_sources" | "read_pdf_source")
            && present(&request["question"])
            && !has(&args, "q")
            && !has(&args, "question")
        {
            args.insert(
                if tool == "look_at_pdf_page" {
                    "question"
                } else {
                    "q"
                }
                .into(),
                request["question"].clone(),
            );
        }
        if !datasets.is_empty() {
            if tool == "search_sources" && !has(&args, "dataset_ids") && !has(&args, "dataset_id") {
                args.insert("dataset_ids".into(), json!(datasets));
            } else if tool != "search_sources" && !has(&args, "dataset_id") && !has(&args, "doc_id")
            {
                args.insert("dataset_id".into(), datasets[0].clone());
            }
        }
        let target = &request["target"];
        if target["match_status"] == "matched" {
            if !has(&args, "doc_id") && !has(&args, "doc_name") {
                args.insert(
                    "doc_name".into(),
                    target
                        .get("file_name")
                        .filter(|v| present(v))
                        .cloned()
                        .unwrap_or(json!("")),
                );
            }
            if !has(&args, "doc_id") && present(&target["dataset_id"]) {
                args.insert("dataset_id".into(), target["dataset_id"].clone());
            }
        }
    }
    let mut call = json!({"tool":tool,"args":args});
    if present(&original["call_id"]) {
        let id = original["call_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| original["call_id"].to_string());
        call["call_id"] = json!(id);
    }
    if tool == "read_pdf_source" {
        super::bind_pdf_source(
            &json!({"call":call,"target":request["target"],"sources":request["sources"]}),
        )
    } else {
        Ok(call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_and_question_defaults_follow_the_explicit_request() {
        let mut request = json!({"call":{"tool":"read_source","args":{}},"dataset_ids":["one","two"],"question":"Read exact section","target":{"match_status":"matched","dataset_id":"two","file_name":"folder/Exact.docx"},"sources":[]});
        let value = augment(&request).unwrap();
        assert_eq!(
            value["args"],
            json!({"q":"Read exact section","dataset_id":"two","doc_name":"folder/Exact.docx"})
        );
        request["call"] = json!({"tool":"search_sources","args":{"q":"model selected query"}});
        assert_eq!(
            augment(&request).unwrap()["args"]["q"],
            "model selected query"
        );
        assert_eq!(
            augment(&request).unwrap()["args"]["dataset_ids"],
            json!(["one", "two"])
        );
    }
    #[test]
    fn explicit_id_query_revision_and_call_id_are_not_overwritten() {
        let request = json!({"call":{"tool":"read_source","call_id":"call-1","args":{"doc_id":"catalog-id","q":"literal","dataset_id":"explicit"}},"dataset_ids":["other"],"question":"different","target":{"match_status":"matched","dataset_id":"target","file_name":"other.docx"},"sources":[]});
        let value = augment(&request).unwrap();
        assert_eq!(value, request["call"]);
    }
    #[test]
    fn pdf_question_is_not_used_as_literal_page_lookup() {
        let request = json!({"call":{"tool":"read_pdf_source","args":{"page":2}},"dataset_ids":["ds"],"question":"What are the requirements?","target":{"match_status":"matched","dataset_id":"ds","file_name":"Plan.pdf"},"sources":[{"dataset_id":"ds","doc_name":"Plan.pdf","source_sha256":"a".repeat(64)}]});
        let value = augment(&request).unwrap();
        assert!(value["args"].get("q").is_none());
        assert_eq!(value["args"]["page"], 2);
        assert_eq!(value["args"]["source_sha256"], "a".repeat(64));
    }
}
