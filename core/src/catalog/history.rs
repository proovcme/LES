//! Durable chat rows shared by streaming, synchronous answers and recovery.
use super::{columns, table, text, Args, Failure, Result};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{json, Value};

const EXTRA_COLUMNS: &[(&str, &str)] = &[
    ("session_id", "TEXT DEFAULT NULL"),
    ("attachment_context", "TEXT DEFAULT ''"),
    ("route_channel", "TEXT DEFAULT ''"),
    ("route_reason", "TEXT DEFAULT ''"),
    ("requested_dataset_filter", "TEXT DEFAULT ''"),
    ("effective_dataset_filter", "TEXT DEFAULT ''"),
    ("resolved_dataset_ids", "TEXT DEFAULT '[]'"),
    ("resolved_dataset_names", "TEXT DEFAULT '[]'"),
    ("source_dataset_ids", "TEXT DEFAULT '[]'"),
    ("source_dataset_names", "TEXT DEFAULT '[]'"),
    ("source_dataset_mismatch", "INTEGER DEFAULT 0"),
    ("query_route_json", "TEXT DEFAULT '{}'"),
    ("retrieval_trace_json", "TEXT DEFAULT '{}'"),
    ("artifact_json", "TEXT DEFAULT '{}'"),
    ("retrieval_quality", "TEXT DEFAULT ''"),
    ("cache_type", "TEXT DEFAULT ''"),
    ("validation_enabled", "INTEGER DEFAULT 1"),
    ("success", "INTEGER DEFAULT 0"),
    ("feedback_status", "TEXT DEFAULT ''"),
    ("feedback_comment", "TEXT DEFAULT ''"),
    ("feedback_correct_answer", "TEXT DEFAULT ''"),
    ("feedback_correct_dataset_filter", "TEXT DEFAULT ''"),
    ("feedback_at", "TEXT DEFAULT NULL"),
    ("feedback_user", "TEXT DEFAULT ''"),
];

fn ensure(conn: &Connection, safe_fallback: &str) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chat_history (
           id INTEGER PRIMARY KEY AUTOINCREMENT, timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
           question TEXT, answer TEXT, sources TEXT, crag_status TEXT,
           latency_sec REAL, tokens INTEGER, session_id TEXT DEFAULT NULL
         )",
    )?;
    let existing = columns(conn, "chat_history")?;
    for (name, ddl) in EXTRA_COLUMNS {
        if !existing.iter().any(|column| column == name) {
            conn.execute(
                &format!("ALTER TABLE chat_history ADD COLUMN {name} {ddl}"),
                [],
            )?;
        }
    }
    if !existing.iter().any(|name| name == "success") {
        conn.execute(
            "UPDATE chat_history SET success=1 WHERE crag_status IN ('VERIFIED','UNVALIDATED')
             AND COALESCE(answer,'') <> '' AND answer <> ?",
            [safe_fallback],
        )?;
    }
    Ok(())
}

const SAVE_COLUMNS: &[&str] = &[
    "question",
    "answer",
    "sources",
    "crag_status",
    "latency_sec",
    "tokens",
    "session_id",
    "route_channel",
    "route_reason",
    "requested_dataset_filter",
    "effective_dataset_filter",
    "resolved_dataset_ids",
    "resolved_dataset_names",
    "source_dataset_ids",
    "source_dataset_names",
    "source_dataset_mismatch",
    "query_route_json",
    "retrieval_trace_json",
    "artifact_json",
    "retrieval_quality",
    "cache_type",
    "validation_enabled",
    "success",
    "attachment_context",
];

fn sql_value(value: &Value) -> Result<SqlValue> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .map(SqlValue::Real)
            })
            .ok_or_else(|| Failure::invalid("Invalid history number")),
        _ => Err(Failure::invalid("Invalid history field")),
    }
}

fn save(conn: &Connection, args: &Args) -> Result<Value> {
    let values = args
        .get("row")
        .and_then(Value::as_object)
        .ok_or_else(|| Failure::invalid("Invalid history row"))?;
    let mut params = SAVE_COLUMNS
        .iter()
        .map(|name| {
            values
                .get(*name)
                .ok_or_else(|| Failure::invalid("Missing history field"))
                .and_then(sql_value)
        })
        .collect::<Result<Vec<_>>>()?;
    let id = match args.get("id").filter(|id| !id.is_null()) {
        None => {
            let marks = vec!["?"; SAVE_COLUMNS.len()].join(",");
            conn.execute(
                &format!(
                    "INSERT INTO chat_history ({}) VALUES ({marks})",
                    SAVE_COLUMNS.join(",")
                ),
                params_from_iter(params),
            )?;
            conn.last_insert_rowid()
        }
        Some(id) => {
            let id = id
                .as_i64()
                .ok_or_else(|| Failure::invalid("Invalid history ID"))?;
            params.push(SqlValue::Integer(id));
            params.push(sql_value(&values["session_id"])?);
            let assignments = SAVE_COLUMNS
                .iter()
                .map(|name| format!("{name}=?"))
                .collect::<Vec<_>>()
                .join(",");
            if conn.execute(
                &format!("UPDATE chat_history SET {assignments} WHERE id=? AND session_id=? AND crag_status!='FAILED'"),
                params_from_iter(params),
            )? != 1
            {
                return Err(Failure::missing("Не найдена строка истории этого диалога."));
            }
            id
        }
    };
    Ok(json!(id))
}

fn interrupt(conn: &Connection, id: Option<&Value>) -> Result<Value> {
    if !table(conn, "chat_history")? {
        return Ok(json!(0));
    }
    const INTERRUPT: &str =
        "UPDATE chat_history SET crag_status='INTERRUPTED',success=0,validation_enabled=0
         WHERE crag_status='IN_PROGRESS'";
    let changed = match id.filter(|id| !id.is_null()) {
        Some(id) => conn.execute(
            &format!("{INTERRUPT} AND id=?"),
            [id.as_i64()
                .ok_or_else(|| Failure::invalid("Invalid history ID"))?],
        )?,
        None => conn.execute(INTERRUPT, [])?,
    };
    Ok(json!(changed))
}

fn feedback(conn: &Connection, args: &Args) -> Result<Value> {
    let id = super::integer(args, "id", 0)?;
    if conn.execute(
        "UPDATE chat_history SET feedback_status=?,feedback_comment=?,feedback_correct_answer=?,
         feedback_correct_dataset_filter=?,feedback_at=CURRENT_TIMESTAMP,feedback_user=? WHERE id=?",
        params![text(args,"feedback")?,text(args,"comment")?,text(args,"correct_answer")?,
            text(args,"correct_dataset_filter")?,text(args,"feedback_user")?,id],
    )? == 0 {
        return Ok(Value::Null);
    }
    super::first(conn, "SELECT id,feedback_status,feedback_comment,feedback_correct_dataset_filter,
        feedback_at,question,answer,sources,crag_status,requested_dataset_filter,effective_dataset_filter,
        source_dataset_names,source_dataset_mismatch,route_channel,route_reason,retrieval_quality,
        cache_type,validation_enabled FROM chat_history WHERE id=?", [id])
}

pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    if matches!(
        action,
        "chat_history_http_messages" | "chat_history_http_learning"
    ) {
        // Same legacy-row migration as the compatibility API, without importing its runtime.
        ensure(
            conn,
            concat!(
                "Система безопасности (Т.О.С.К.А.) не смогла подтвердить ответ из базы знаний. ",
                "Попробуйте переформулировать вопрос или выбрать другой датасет."
            ),
        )?;
        return if action == "chat_history_http_messages" {
            super::history_view::messages(conn, args)
        } else {
            super::history_view::learning(conn, args)
        };
    }
    match action {
        "chat_history_init"
        | "chat_history_begin"
        | "chat_history_save"
        | "chat_history_feedback" => {
            ensure(conn, text(args, "safe_fallback")?)?;
        }
        _ => {}
    }
    match action {
        "chat_history_init" => Ok(Value::Null),
        "chat_history_sessions" => super::history_view::sessions(conn, args),
        "chat_history_learning" => super::history_view::learning(conn, args),
        "chat_history_messages" => super::history_view::messages(conn, args),
        "chat_history_begin" => {
            conn.execute(
                "INSERT INTO chat_history (session_id,question,answer,crag_status,success,validation_enabled,attachment_context)
                 VALUES (?,?,'','IN_PROGRESS',0,0,?)",
                params![text(args,"session_id")?, text(args,"question")?, text(args,"attachment_context")?],
            )?;
            Ok(json!(conn.last_insert_rowid()))
        }
        "chat_history_save" => save(conn, args),
        "chat_history_feedback" => feedback(conn, args),
        "chat_history_checkpoint" => {
            let id = super::integer(args, "id", 0)?;
            match text(args, "event")? {
                "reset" => conn.execute(
                    "UPDATE chat_history SET answer='',tokens=0 WHERE id=? AND crag_status='IN_PROGRESS'", [id],
                )?,
                "token" => conn.execute(
                    "UPDATE chat_history SET answer=COALESCE(answer,'')||?,tokens=COALESCE(tokens,0)+1
                     WHERE id=? AND crag_status='IN_PROGRESS'", params![text(args,"text")?, id],
                )?,
                "error" => {
                    let error=args.get("error").and_then(Value::as_object).ok_or_else(||Failure::invalid("Invalid public error"))?;
                    let code=error.get("code").and_then(Value::as_str).filter(|v|!v.is_empty() && v.len()<=100 && v.bytes().all(|b|b.is_ascii_uppercase() || b.is_ascii_digit() || b==b'_'))
                        .ok_or_else(||Failure::invalid("Invalid public error code"))?;
                    let detail=error.get("detail").and_then(Value::as_str).filter(|v|!v.is_empty() && v.chars().count()<=2048)
                        .ok_or_else(||Failure::invalid("Invalid public error message"))?;
                    let artifact=json!({"error":{"code":code,"detail":detail},"completion_status":"failed"});
                    conn.execute("UPDATE chat_history SET crag_status='FAILED',success=0,validation_enabled=0,artifact_json=?
                        WHERE id=? AND crag_status='IN_PROGRESS'",params![artifact.to_string(),id])?
                },
                _ => return Err(Failure::invalid("Invalid history event")),
            };
            Ok(Value::Null)
        }
        "chat_history_interrupt" => interrupt(conn, args.get("id")),
        _ => Err(Failure::invalid("Invalid history action")),
    }
}
