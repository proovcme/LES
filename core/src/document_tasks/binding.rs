//! The same request-bound table tool is used by native chat and the classic codec.
use super::*;
pub(super) fn context(conn: &Connection, state: &Value) -> Result<Value> {
    let owner = text(state, "owner")?;
    let id = text(state, "task_id")?;
    let status = status(conn, owner, id)?;
    let current = &state["current"];
    let next = if current["packet_id"].is_string()
        && current["rows"].as_array().is_some_and(|r| !r.is_empty())
    {
        "record"
    } else if status["pending"].as_u64().unwrap_or(0) > 0 {
        "next"
    } else {
        "finish_or_read_results"
    };
    Ok(
        json!({"table_task":status,"current_read":current,"task_is_for_current_question":task(conn,owner,id)?["question"]==state["question"],
        "next_action_for_active_task":next,"instruction":"Use table_document for exact cells and saved results. Never infer completion from a summary. coverage_complete only counts row decisions; it does not certify their correctness. If task_is_for_current_question is true, the task is ALREADY started: do not call start again. Read with next, decide every row, save with record, then next until pending is zero. Continue a prior task only when the user asks to continue it; for a different objective call start once."}),
    )
}
pub(super) fn execute(conn: &Connection, r: &Value) -> Result<Value> {
    let mut state = r["state"].clone();
    let args = &r["arguments"];
    let op = text(args, "operation")?;
    let owner = text(&state, "owner")?.to_owned();
    let id = args["task_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(text(&state, "task_id")?)
        .to_owned();
    let target = task(conn, &owner, &id)?;
    if target["scope_key"].as_str().is_some_and(|s| !s.is_empty()) {
        conn.execute(
            "INSERT OR REPLACE INTO document_bindings VALUES(?,?)",
            params![target["scope_key"].as_str(), id],
        )?;
    }
    if state["task_id"] != id {
        state["current"] = json!({});
    }
    state["task_id"] = json!(id);
    if ["start", "next", "record"].contains(&op) {
        state["work_requested"] = json!(true);
    }
    let mut input = args.clone();
    input["owner"] = json!(owner);
    input["task_id"] = json!(id);
    input["max_bytes"] = state["packet_bytes"].clone();
    let payload = match op {
        "start" => {
            if target["question"] == state["question"] {
                let next = if state["current"]["packet_id"].is_string()
                    && state["current"]["rows"]
                        .as_array()
                        .is_some_and(|r| !r.is_empty())
                {
                    "record"
                } else {
                    "next"
                };
                return Err(Failure::invalid(&format!("Task already started for this question. Do not call start again. Use {next}; current packet and saved results are preserved.")));
            }
            state["task_id"] = open(
                conn,
                &owner,
                text(&state, "question")?,
                &source(conn, &owner, &id)?,
                target["scope_key"].as_str().unwrap_or(""),
            )?;
            json!({"started_for_current_question":true})
        }
        "status" => json!({"recent_tasks":operate(conn,"recent",&input)?}),
        "next" => operate(conn, "next_packet", &input)?,
        "record" => {
            if input["packet_id"].as_str().is_none_or(|s| s.is_empty()) {
                input["packet_id"] = state["current"]["packet_id"].clone();
            }
            operate(conn, "commit", &input)?;
            json!({"saved":true})
        }
        "results" => operate(conn, "results", &input)?,
        "read_window" => operate(conn, "read_window", &input)?,
        "read_cell" => {
            input["max_chars"] =
                json!((state["packet_bytes"].as_i64().unwrap_or(6000) / 4).clamp(128, 3000));
            operate(conn, "read_cell", &input)?
        }
        _ => return Err(Failure::invalid("Неизвестное действие с таблицей")),
    };
    if ["read_window", "read_cell"].contains(&op) {
        state["current"]["addressed_read"] = payload.clone();
    } else {
        state["current"] = payload.clone();
    }
    let mut result = status(conn, &owner, text(&state, "task_id")?)?;
    result
        .as_object_mut()
        .unwrap()
        .extend(payload.as_object().cloned().unwrap_or_default());
    Ok(json!({"state":state,"payload":result}))
}
