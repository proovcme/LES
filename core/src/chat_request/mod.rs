//! Public chat request normalization. Model choice and source scope never come from prose.
pub mod journal;
mod scope;
#[cfg(test)]
mod tests;
use serde_json::{json, Value};
pub type Result<T> = std::result::Result<T, &'static str>;
pub fn clause_heading(heading: &str, number: &str) -> Result<bool> {
    let parts = number.split('.').collect::<Vec<_>>();
    if !(2..=6).contains(&parts.len())
        || parts
            .iter()
            .any(|s| s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("SOURCE_CLAUSE_INVALID");
    }
    let mut words = heading.trim().trim_start_matches('#').split_whitespace();
    let mut token = words.next().unwrap_or("");
    if token.eq_ignore_ascii_case("пункт") || token.to_lowercase() == "пункт" {
        token = words.next().unwrap_or("");
    }
    Ok(token.trim_end_matches('.') == number)
}
pub fn validate(input: &Value) -> Result<Value> {
    let mut request = input.as_object().cloned().ok_or("CHAT_REQUEST_INVALID")?;
    let nullable = [
        "dataset_ids",
        "dataset_filter",
        "reranker_enabled",
        "semantic_cache_enabled",
        "validation_enabled",
        "session_id",
        "project_id",
        "scope",
        "selected_sources_only",
        "output_directive",
        "response_length",
        "mode",
        "profile_revision_id",
        "attachment_context",
        "attachment_id",
        "target_file",
        "target_files",
        "provider_config",
    ];
    request.retain(|key, _| {
        key == "question"
            || key == "apply_profile_revision"
            || key == "candidate_acceptance"
            || nullable.contains(&key.as_str())
    });
    for key in nullable {
        request.entry(key).or_insert(Value::Null);
    }
    for key in ["apply_profile_revision", "candidate_acceptance"] {
        request.entry(key).or_insert(json!(false));
    }
    for (field, limit, required) in [
        ("question", 20000, true),
        ("attachment_context", 20000, false),
        ("session_id", 512, false),
        ("target_file", 1000, false),
        ("output_directive", 16000, false),
        ("profile_revision_id", 256, false),
        ("mode", 64, false),
        ("dataset_filter", 1000, false),
    ] {
        match request.get(field).filter(|v| !v.is_null()) {
            Some(Value::String(text)) => {
                let text = text.trim();
                if text.chars().count() > limit {
                    return Err("CHAT_TEXT_LIMIT");
                }
                if required && text.is_empty() {
                    return Err("CHAT_QUESTION_REQUIRED");
                }
                let text = if field == "target_file" {
                    text.replace('\\', "/")
                } else {
                    text.into()
                };
                request.insert(
                    field.into(),
                    if text.is_empty() && !required {
                        Value::Null
                    } else {
                        json!(text)
                    },
                );
            }
            None if !required => {}
            None => return Err("CHAT_QUESTION_REQUIRED"),
            _ => return Err("CHAT_REQUEST_INVALID"),
        }
    }
    for field in [
        "reranker_enabled",
        "semantic_cache_enabled",
        "validation_enabled",
        "selected_sources_only",
        "apply_profile_revision",
        "candidate_acceptance",
    ] {
        if request
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err("CHAT_REQUEST_INVALID");
        }
    }
    if request
        .get("project_id")
        .is_some_and(|v| !v.is_null() && v.as_i64().is_none())
    {
        return Err("CHAT_PROJECT_INVALID");
    }
    for field in ["dataset_ids", "target_files"] {
        let Some(values) = request.get(field).filter(|v| !v.is_null()) else {
            continue;
        };
        let values = values
            .as_array()
            .filter(|v| v.len() <= 32766)
            .ok_or("CHAT_SCOPE_INVALID")?;
        let mut normalized = Vec::new();
        for value in values {
            let text = value.as_str().ok_or("CHAT_SCOPE_INVALID")?.trim();
            let text = if field == "target_files" {
                text.replace('\\', "/")
            } else {
                text.into()
            };
            if text.chars().count() > 1000 {
                return Err("CHAT_TEXT_LIMIT");
            }
            if !text.is_empty() && !normalized.contains(&text) {
                normalized.push(text);
            }
        }
        request.insert(field.into(), json!(normalized));
    }
    if let Some(id) = request.get("attachment_id").filter(|v| !v.is_null()) {
        let id = id
            .as_str()
            .ok_or("CHAT_ATTACHMENT_INVALID")?
            .trim()
            .to_ascii_lowercase();
        if id.len() != 17
            || !id.starts_with("read_")
            || !id[5..].bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("CHAT_ATTACHMENT_INVALID");
        }
        request.insert("attachment_id".into(), json!(id));
    }
    if let Some(length) = request.get("response_length").filter(|v| !v.is_null()) {
        let length = length
            .as_str()
            .ok_or("CHAT_REQUEST_INVALID")?
            .trim()
            .to_ascii_lowercase();
        if !["short", "standard", "detailed", "maximum"].contains(&length.as_str()) {
            return Err("CHAT_RESPONSE_LENGTH_INVALID");
        }
        request.insert("response_length".into(), json!(length));
    }
    if request
        .get("scope")
        .is_some_and(|v| !v.is_null() && !v.is_object())
    {
        return Err("CHAT_SCOPE_INVALID");
    }
    Ok(Value::Object(request))
}
pub fn resolve_scope(request: &Value, datasets: &[Value], projects: &[Value]) -> Result<Value> {
    scope::resolve(request, datasets, projects)
}
pub fn execute(request: &Value) -> Result<Value> {
    match request["action"].as_str() {
        Some("validate") => validate(&request["input"]),
        Some("scope") => resolve_scope(
            &request["input"],
            request["datasets"].as_array().ok_or("CHAT_SCOPE_INVALID")?,
            request["projects"].as_array().ok_or("CHAT_SCOPE_INVALID")?,
        ),
        _ => Err("CHAT_REQUEST_INVALID"),
    }
}
