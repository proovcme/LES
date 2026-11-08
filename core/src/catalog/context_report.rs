//! Public projection of persisted context diagnostics. Never return raw traces.
use super::*;
use rusqlite::params;

pub(super) fn has_turn(conn: &Connection, sid: &str, id: i64) -> Result<bool> {
    if !table(conn, "chat_history")? {
        return Ok(false);
    }
    let fields = columns(conn, "chat_history")?;
    if !["id", "session_id"]
        .iter()
        .all(|name| fields.iter().any(|field| field == name))
    {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM chat_history WHERE session_id=? AND id=?)",
        params![sid, id],
        |row| row.get(0),
    )?)
}

pub(super) fn read(conn: &Connection, sid: &str, id: i64) -> Result<Value> {
    if !has_turn(conn, sid, id)? {
        return Err(Failure::missing("Сообщение в этом чате не найдено."));
    }
    if !columns(conn, "chat_history")?
        .iter()
        .any(|field| field == "retrieval_trace_json")
    {
        return Ok(json!({"calls":[]}));
    }
    let trace: Option<String> = conn.query_row(
        "SELECT retrieval_trace_json FROM chat_history WHERE session_id=? AND id=?",
        params![sid, id],
        |row| row.get(0),
    )?;
    let trace: Value = trace
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    let calls: Vec<_> = items(&trace["context_governor"]["calls"])
        .filter_map(project_call)
        .collect();
    Ok(json!({"calls":calls}))
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn project_call(call: &Value) -> Option<Value> {
    let purpose = call["purpose"].as_str()?;
    if !matches!(purpose, "answer" | "tool_decision") {
        return None;
    }
    let delivery = match call["delivery"].as_str() {
        Some("confirmed") => "confirmed",
        Some("prepared") => "prepared",
        _ if call["sent_to_model"] == true => "legacy",
        _ => "prepared",
    };
    let budget = call["input_budget_tokens"].as_u64()?;
    let used = call["included_tokens"].as_u64()?;
    let sections: Vec<_> = items(&call["sections"])
        .filter_map(|row| {
            let kind = row["kind"].as_str()?;
            if !crate::context::ORDER.contains(&kind) {
                return None;
            }
            Some(json!({"kind":kind,"tokens":row["tokens"].as_u64()?}))
        })
        .collect();
    let omissions: Vec<_> = items(&call["omissions"])
        .filter_map(|row| {
            let kind = row["kind"].as_str()?;
            if !crate::context::ORDER.contains(&kind) || row["reason"] != "budget_exhausted" {
                return None;
            }
            Some(json!({"kind":kind,"count":row["omitted"].as_u64()?,"reason":"budget_exhausted"}))
        })
        .collect();
    let sources: Vec<_> = items(&call["visible_sources"])
        .filter_map(|source| project_source(source, call))
        .collect();
    let omitted_sources: Vec<_> = items(&call["omitted_sources"])
        .filter_map(|source| {
            Some(json!({"index":source["index"].as_u64()?,"doc_name":source["doc_name"].as_str()?}))
        })
        .collect();
    Some(json!({
        "purpose":purpose,"delivery":delivery,"produced_answer":call["produced_answer"] == true,
        "used":used,"budget":budget,"free":budget.saturating_sub(used),
        "generation_reserve":call["generation_reserve_tokens"].as_u64(),
        "safety_reserve":call["safety_reserve_tokens"].as_u64(),
        "provider_prompt_tokens":call["provider_prompt_tokens"].as_u64(),
        "sections":sections,"omissions":omissions,"sources":sources,"omitted_sources":omitted_sources,
    }))
}

fn project_source(source: &Value, call: &Value) -> Option<Value> {
    let index = source["index"].as_u64()?;
    let mut result = scalar_fields(
        source,
        &[
            "doc_name",
            "doc_id",
            "dataset_id",
            "label",
            "source_page",
            "page",
            "page_number",
        ],
    );
    result.insert("index".into(), index.into());
    result.insert(
        "locator".into(),
        Value::Object(scalar_fields(
            &source["locator"],
            &[
                "sheet",
                "row",
                "cell",
                "anchor",
                "source_sha256",
                "section_heading",
            ],
        )),
    );
    let prefix = format!("[Источник {index}");
    let evidence = items(&call["sections"])
        .filter(|section| section["kind"] == "evidence")
        .flat_map(|section| items(&section["objects"]))
        .filter_map(|object| object["text"].as_str())
        .find(|text| {
            text.strip_prefix(&prefix)
                .is_some_and(|tail| tail.starts_with([' ', ']']))
        });
    if let Some(evidence) = evidence {
        let mut chars = evidence.chars();
        let excerpt: String = chars.by_ref().take(12_000).collect();
        result.insert("model_excerpt".into(), excerpt.into());
        result.insert("excerpt_limited".into(), chars.next().is_some().into());
    }
    Some(Value::Object(result))
}

fn scalar_fields(value: &Value, keys: &[&str]) -> Map<String, Value> {
    keys.iter()
        .filter_map(|&key| {
            value
                .get(key)
                .filter(|v| v.is_string() || v.is_u64())
                .map(|v| (key.to_owned(), v.clone()))
        })
        .collect()
}
