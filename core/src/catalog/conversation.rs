use super::*;
use rusqlite::params;

pub(super) fn defaults() -> Value {
    json!({"summary":"","through_id":0,"cutoff_id":0,"enabled":true,"auto_summary":true,"revision":0,"updated_at":0.0,"model_revision":"","last_error":""})
}
fn get(conn: &Connection, sid: &str) -> Result<Value> {
    let mut result = defaults();
    if table(conn, "les_conversation_context")? {
        if let Some(raw) = conn
            .query_row(
                "SELECT state_json FROM les_conversation_context WHERE session_id=?",
                [sid],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            let value: Value = serde_json::from_str(&raw).map_err(|_| {
                Failure::new(
                    "WORKSPACE_UNAVAILABLE",
                    "Не удалось прочитать состояние памяти. Откройте диагностику.",
                )
            })?;
            result.as_object_mut().unwrap().extend(
                value
                    .as_object()
                    .ok_or_else(|| Failure::invalid("Invalid saved context"))?
                    .clone(),
            );
        }
    }
    Ok(result)
}
fn history(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    if !table(conn, "chat_history")? {
        return Ok(json!([]));
    }
    let present = columns(conn, "chat_history")?;
    let has = |s: &str| present.iter().any(|c| c == s);
    let required = match action {
        "history_recall" => vec![
            "id",
            "question",
            "answer",
            "feedback_status",
            "success",
            "crag_status",
        ],
        "history_questions" => vec!["id", "session_id", "question"],
        "history_traces" => vec!["id", "session_id", "retrieval_trace_json"],
        _ => vec!["id", "session_id", "question", "answer"],
    };
    if !required.iter().all(|s| has(s)) {
        return Ok(json!([]));
    }
    if action == "history_recall" {
        return Ok(json!(rows(conn,"SELECT id,question,answer,feedback_status FROM chat_history WHERE success=1 AND crag_status IN ('VERIFIED','DETERMINISTIC') AND feedback_status NOT IN ('bad_answer','incorrect','wrong_dataset','bad_source') AND question != ? ORDER BY id DESC LIMIT ?",params![text(args,"question")?,integer(args,"limit",300)?])?));
    }
    let after = integer(args, "after_id", 0)?;
    let sid = text(args, "session_id")?;
    let success = if has("success") { " AND success=1" } else { "" };
    if action == "context_history" {
        return Ok(json!(rows(conn,&format!("SELECT id,question,answer FROM chat_history WHERE session_id=? AND id>?{success} ORDER BY id"),params![sid,after])?));
    }
    let fields = match action {
        "history_questions" => "question".to_string(),
        "history_traces" => "retrieval_trace_json".to_string(),
        _ => format!(
            "id,question,answer,{} AS attachment_context,{} AS retrieval_trace_json",
            if has("attachment_context") {
                "attachment_context"
            } else {
                "''"
            },
            if has("retrieval_trace_json") {
                "retrieval_trace_json"
            } else {
                "'{}'"
            }
        ),
    };
    let completed = if action == "history_session" && has("crag_status") {
        " AND COALESCE(crag_status,'') NOT IN ('IN_PROGRESS','INTERRUPTED')"
    } else {
        ""
    };
    let mut values=rows(conn,&format!("SELECT {fields} FROM chat_history WHERE session_id=? AND id>?{completed} ORDER BY id DESC LIMIT ?"),params![sid,after,integer(args,"limit",6)?])?;
    values.reverse();
    Ok(json!(values))
}
pub(super) fn status(conn: Option<&Connection>, args: &Args) -> Result<Value> {
    let sid = text(args, "session_id")?;
    let mut value = conn.map_or_else(|| Ok(defaults()), |c| get(c, sid))?;
    let mut counts = (0i64, 0i64);
    if let Some(conn) = conn {
        if table(conn, "chat_history")? {
            let present = columns(conn, "chat_history")?;
            if ["id", "session_id", "question", "answer"]
                .iter()
                .all(|s| present.iter().any(|c| c == s))
            {
                let success = if present.iter().any(|s| s == "success") {
                    " AND success=1"
                } else {
                    ""
                };
                counts=conn.query_row(&format!("SELECT count(*),COALESCE(sum(id<=?),0) FROM chat_history WHERE session_id=? AND id>?{success}"),params![value["through_id"].as_i64().unwrap_or(0),sid,value["cutoff_id"].as_i64().unwrap_or(0)],|r|Ok((r.get(0)?,r.get(1)?)))?;
            }
        }
    }
    value.as_object_mut().unwrap().extend(json!({"session_id":sid,"remembered_turns":counts.0,"summarized_turns":counts.1,"is_evidence":false,"history_preserved":true}).as_object().unwrap().clone());
    Ok(value)
}
pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    if action.starts_with("history_") || action == "context_history" {
        return history(conn, action, args);
    }
    let sid = text(args, "session_id")?;
    if boolean(args, "check_session", false)?
        && crate::workspace::records::get(conn, sid)?.is_none()
    {
        return Err(Failure::missing("Чат не найден"));
    }
    if action == "context_status" {
        return status(Some(conn), args);
    }
    let mut value = get(conn, sid)?;
    if action == "context_get" {
        return Ok(value);
    }
    let revision = integer(args, "expected_revision", -1)?;
    if revision < 0 {
        return Err(Failure::invalid("Invalid expected_revision"));
    }
    if value["revision"].as_i64() != Some(revision) {
        return Err(Failure::conflict(
            "Память изменилась в другом окне. Обновите её перед сохранением.",
        ));
    }
    let mut changes = args
        .get("changes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if action == "context_forget" {
        let history = history(conn, "context_history", args)?;
        let cutoff = history
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_i64())
            .max()
            .unwrap_or(0);
        changes=json!({"summary":"","through_id":cutoff,"cutoff_id":cutoff,"last_error":"","model_revision":""}).as_object().unwrap().clone();
    }
    let allowed = defaults();
    if changes.keys().any(|k| allowed.get(k).is_none()) {
        return Err(Failure::invalid("Unknown context field"));
    }
    value.as_object_mut().unwrap().extend(changes);
    value["revision"] = revision
        .checked_add(1)
        .ok_or_else(|| Failure::invalid("Context revision exhausted"))?
        .into();
    value["updated_at"] = json!(seconds());
    conn.execute_batch("CREATE TABLE IF NOT EXISTS les_conversation_context(session_id TEXT PRIMARY KEY,state_json TEXT NOT NULL);")?;
    conn.execute("INSERT INTO les_conversation_context VALUES (?,?) ON CONFLICT(session_id) DO UPDATE SET state_json=excluded.state_json",params![sid,value.to_string()])?;
    Ok(value)
}
