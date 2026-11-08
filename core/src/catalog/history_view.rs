//! History projection shared by API clients. No regeneration, inference or error rewriting.
use super::{rows, table, Args, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

fn object(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn list(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
        .filter(Value::is_array)
        .unwrap_or_else(|| json!([]))
}
pub(super) fn sessions(conn: &Connection, args: &Args) -> Result<Value> {
    if !table(conn, "chat_history")? {
        return Ok(json!([]));
    }
    let mut records=rows(conn,"SELECT session_id,MIN(timestamp) AS started_at,MAX(timestamp) AS last_at,COUNT(*) AS msg_count,MIN(question) AS first_question FROM chat_history WHERE session_id IS NOT NULL GROUP BY session_id ORDER BY last_at DESC LIMIT ?",[super::integer(args,"limit",50)?])?;
    for row in &mut records {
        row["first_question"] = json!(string(&row["first_question"])
            .chars()
            .take(120)
            .collect::<String>());
    }
    Ok(json!(records))
}
pub(super) fn learning(conn: &Connection, args: &Args) -> Result<Value> {
    let mut records=rows(conn,"SELECT id,timestamp,question,answer,crag_status,sources,requested_dataset_filter,effective_dataset_filter,resolved_dataset_names,source_dataset_names,source_dataset_mismatch,route_channel,route_reason,retrieval_quality,feedback_status,feedback_comment,feedback_correct_dataset_filter FROM chat_history WHERE (?1 AND feedback_status='correct') OR (NOT ?1 AND (success=1 OR feedback_status IN ('correct','partial','bad_answer','incorrect','wrong_dataset','bad_source'))) ORDER BY id DESC LIMIT ?2",params![super::boolean(args,"confirmed_only",false)?,super::integer(args,"limit",100)?])?;
    for row in &mut records {
        row["sources"] = json!(string(&row["sources"])
            .split(',')
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>());
        for key in [
            "requested_dataset_filter",
            "effective_dataset_filter",
            "route_channel",
            "route_reason",
            "retrieval_quality",
            "feedback_status",
            "feedback_comment",
            "feedback_correct_dataset_filter",
        ] {
            row[key] = json!(string(&row[key]));
        }
        for key in ["resolved_dataset_names", "source_dataset_names"] {
            row[key] = list(&row[key]);
        }
        row["source_dataset_mismatch"] =
            json!(row["source_dataset_mismatch"].as_i64().unwrap_or(0) != 0);
    }
    Ok(json!(records))
}
fn source_map(trace: &Value) -> Value {
    if let Some(direct) = trace["source_map"].as_array() {
        return json!(direct.iter().filter(|v| v.is_object()).collect::<Vec<_>>());
    }
    let empty = Vec::new();
    let visible = trace["evidence_manifest"]["model_visible"]
        .as_array()
        .unwrap_or(&empty);
    let restored=visible.iter().enumerate().filter(|(_,v)|v.is_object()).map(|(i,v)|{
        let locator=v.get("locator").filter(|v|v.is_object()).cloned().unwrap_or_else(||json!({}));
        json!({"index":i+1,"evidence_ref":v.get("id").cloned().unwrap_or_else(||json!(format!("S{}",i+1))),
            "doc_name":v["doc_name"],"doc_id":v["doc_id"],"dataset_id":v["dataset_id"],
            "source_ref":locator["source_ref"],"snippet":locator["excerpt"],"locator":locator})
    }).collect::<Vec<_>>();
    if !restored.is_empty() {
        return json!(restored);
    }
    let mut sources: Vec<Value> = Vec::new();
    for payload in trace["tool_loop"]["results"].as_array().unwrap_or(&empty) {
        if !matches!(string(&payload["tool"]), "web_search" | "web_read") {
            continue;
        }
        let result = &payload["result"];
        for source in payload["sources"].as_array().unwrap_or(&empty) {
            let url = source["url"]
                .as_str()
                .or_else(|| result["final_url"].as_str())
                .unwrap_or("");
            if url.is_empty() {
                continue;
            }
            let row = result["results"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .find(|r| r["url"] == url);
            let title = source["title"]
                .as_str()
                .or_else(|| result["title"].as_str())
                .or_else(|| row.and_then(|r| r["title"].as_str()))
                .unwrap_or("");
            let snippet = result["text"]
                .as_str()
                .or_else(|| row.and_then(|r| r["snippet"].as_str()))
                .unwrap_or("");
            let value = json!({"doc_name":title,"source_ref":url,"snippet":snippet,"locator":{
                "kind":"web_result","url":url,"title":title,"provider":result["provider"],"excerpt":snippet}});
            if let Some(existing) = sources.iter_mut().find(|r| r["source_ref"] == url) {
                *existing = value;
            } else {
                sources.push(value);
            }
        }
    }
    for (i, v) in sources.iter_mut().enumerate() {
        v["index"] = json!(i + 1);
        v["label"] = json!(format!("Источник {}", i + 1));
    }
    json!(sources)
}

pub(super) fn messages(conn: &Connection, args: &Args) -> Result<Value> {
    let sid = args.get("session_id").and_then(Value::as_str).unwrap_or("");
    let mut records = if !sid.is_empty() {
        rows(
            conn,
            "SELECT * FROM chat_history WHERE session_id=? ORDER BY id ASC",
            [sid],
        )?
    } else {
        let limit = super::integer(args, "limit", 40)?.clamp(1, 200);
        rows(
            conn,
            "SELECT * FROM chat_history ORDER BY id DESC LIMIT ?",
            params![limit],
        )?
    };
    if sid.is_empty() {
        records.reverse();
    }
    let mut messages = Vec::new();
    for row in records {
        let id = &row["id"];
        let timestamp = &row["timestamp"];
        messages.push(
            json!({"role":"user","text":row["question"],"history_id":id,"requested_at":timestamp}),
        );
        let trace = object(&row["retrieval_trace_json"]);
        let artifact = object(&row["artifact_json"]);
        let phases = trace
            .get("latency_phases")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut meta = json!({"execution":trace["execution"],"history_id":id,"query_route":object(&row["query_route_json"]),
            "retrieval_trace":trace,"cache":row["cache_type"].as_str().filter(|v|!v.is_empty()).unwrap_or("miss"),
            "validation":{"enabled":row["validation_enabled"].as_i64().unwrap_or(0)!=0},"requested_at":timestamp,
            "latency_phases":phases,"elapsed_sec":phases["wall_total"],"model_think_sec":phases["generation"],
            "source_map":source_map(&trace),"source_counts":trace.get("source_counts").cloned().unwrap_or_else(||json!({}))});
        if !artifact.as_object().unwrap().is_empty() {
            meta["artifact"] = artifact.clone();
        }
        let status = string(&row["crag_status"]);
        let answer = string(&row["answer"]);
        if artifact["error"].is_object() {
            meta["error"] = artifact["error"].clone();
            meta["completion_status"] = json!(if status == "FAILED" {
                "failed"
            } else {
                "interrupted"
            });
            meta["partial"] = json!(!answer.is_empty());
        } else if matches!(status, "IN_PROGRESS" | "INTERRUPTED") {
            meta["partial"] = json!(true);
            meta["completion_status"] = json!(if status == "IN_PROGRESS" {
                "in_progress"
            } else {
                "interrupted"
            });
        }
        if row["feedback_status"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
        {
            meta["feedback"] = row["feedback_status"].clone();
        }
        let sources = string(&row["sources"])
            .split(',')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        messages.push(json!({"role":"ai","text":answer,"history_id":id,"srcs":sources,"crag":status,"meta":meta}));
    }
    Ok(json!(messages))
}
