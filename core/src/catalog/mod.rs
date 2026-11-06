//! Existing project/note/context tables, shared transaction owner; no model decisions.
mod conversation;
mod history;
mod intake;
mod job_store;
mod library;
mod model_connection;
mod notes;
mod projects;

use crate::workspace::{connection, Failure, Result};
use rusqlite::{types::ValueRef, Connection, OptionalExtension, Params};
use serde_json::{json, Map, Value};
use std::path::Path;

type Args = Map<String, Value>;
fn text<'a>(args: &'a Args, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::invalid(&format!("Invalid {key}")))
}
fn integer(args: &Args, key: &str, default: i64) -> Result<i64> {
    args.get(key)
        .filter(|v| !v.is_null())
        .map_or(Ok(default), |v| {
            v.as_i64()
                .ok_or_else(|| Failure::invalid(&format!("Invalid {key}")))
        })
}
fn boolean(args: &Args, key: &str, default: bool) -> Result<bool> {
    args.get(key)
        .filter(|v| !v.is_null())
        .map_or(Ok(default), |v| {
            v.as_bool()
                .ok_or_else(|| Failure::invalid(&format!("Invalid {key}")))
        })
}
fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}
fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    // Table names are fixed internal constants, never request input.
    Ok(conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}
fn rows(conn: &Connection, sql: &str, params: impl Params) -> Result<Vec<Value>> {
    let mut statement = conn.prepare(sql)?;
    let names = statement
        .column_names()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let values = statement
        .query_map(params, |row| {
            let mut object = Map::new();
            for (i, name) in names.iter().enumerate() {
                let value =
                    match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(v) => v.into(),
                        ValueRef::Real(v) => json!(v),
                        ValueRef::Text(v) => String::from_utf8(v.to_vec())
                            .map(Value::String)
                            .map_err(|e| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    i,
                                    rusqlite::types::Type::Text,
                                    Box::new(e),
                                )
                            })?,
                        ValueRef::Blob(_) => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                i,
                                name.clone(),
                                rusqlite::types::Type::Blob,
                            ))
                        }
                    };
                object.insert(name.clone(), value);
            }
            Ok(Value::Object(object))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}
fn first(conn: &Connection, sql: &str, params: impl Params) -> Result<Value> {
    Ok(rows(conn, sql, params)?
        .into_iter()
        .next()
        .unwrap_or(Value::Null))
}
fn seconds() -> f64 {
    chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0
}

pub fn execute(request: &Value) -> Value {
    match dispatch(request) {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(e) => {
            json!({"status":"failed","code":e.code,"detail":e.detail,"sqlite_extended":e.sqlite_extended})
        }
    }
}
fn dispatch(request: &Value) -> Result<Value> {
    if request["schema"] != "les.catalog.v1" {
        return Err(Failure::invalid("Invalid catalog schema"));
    }
    let path = Path::new(
        request["database"]
            .as_str()
            .ok_or_else(|| Failure::invalid("Invalid database path"))?,
    );
    if !path.is_absolute() {
        return Err(Failure::invalid("Absolute database path required"));
    }
    let action = request["action"]
        .as_str()
        .ok_or_else(|| Failure::invalid("Invalid catalog action"))?;
    let args = request["arguments"]
        .as_object()
        .ok_or_else(|| Failure::invalid("Invalid catalog arguments"))?;
    let write = match action {
        "dataset_create"
        | "intake_enqueue"
        | "intake_claim"
        | "intake_manifest"
        | "intake_register"
        | "intake_index_checkpoint"
        | "intake_wait"
        | "intake_fail"
        | "project_create"
        | "project_status"
        | "project_delete"
        | "project_link"
        | "project_unlink"
        | "note_create"
        | "note_update"
        | "note_delete"
        | "note_list"
        | "context_update"
        | "context_forget"
        | "job_init"
        | "job_create"
        | "job_update"
        | "job_recover"
        | "chat_history_init"
        | "chat_history_begin"
        | "chat_history_save"
        | "chat_history_feedback"
        | "chat_history_checkpoint"
        | "chat_history_interrupt" => true,
        "library_datasets"
        | "intake_get"
        | "intake_list"
        | "intake_batch"
        | "intake_owned_datasets"
        | "intake_active_index"
        | "intake_has_work"
        | "library_dataset"
        | "library_documents"
        | "library_forest"
        | "project_list"
        | "project_get"
        | "project_links"
        | "project_for_dataset"
        | "project_datasets"
        | "project_metadata"
        | "note_projection"
        | "context_get"
        | "context_status"
        | "context_history"
        | "history_recall"
        | "history_session"
        | "history_questions"
        | "history_traces"
        | "model_execution_snapshot"
        | "job_get"
        | "job_list"
        | "job_active"
        | "job_pending_documents" => false,
        _ => return Err(Failure::invalid("Unknown catalog action")),
    };
    if action == "job_init" {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| {
                Failure::new(
                    "WORKSPACE_UNAVAILABLE",
                    "Не удалось создать каталог заданий.",
                )
            })?;
        }
    }
    let mut conn = connection(path, write)?;
    let Some(conn) = conn.as_mut() else {
        if boolean(args, "check_session", false)? {
            return Err(Failure::missing("Чат не найден"));
        }
        return Ok(match action {
            "project_get" => Value::Null,
            "project_for_dataset" => json!(0),
            "context_get" => conversation::defaults(),
            "context_status" => conversation::status(None, args)?,
            "project_metadata" => json!({}),
            "project_datasets" => projects::datasets(None, args)?,
            "job_pending_documents" => json!(0),
            "model_execution_snapshot" => Value::Null,
            _ => json!([]),
        });
    };
    if action.starts_with("job_") {
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        if write {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
    }
    if write {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let value = operate(&tx, action, args)?;
        tx.commit()?;
        Ok(value)
    } else {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
        let value = operate(&tx, action, args)?;
        tx.commit()?;
        Ok(value)
    }
}
fn operate(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    if action.starts_with("intake_") {
        intake::execute(conn, action, args)
    } else if action.starts_with("library_") || action == "dataset_create" {
        library::execute(conn, action, args)
    } else if action.starts_with("job_") {
        job_store::execute(conn, action, args)
    } else if action.starts_with("chat_history_") {
        history::execute(conn, action, args)
    } else if action == "model_execution_snapshot" {
        model_connection::snapshot(conn, args)
    } else if action.starts_with("project_") {
        projects::execute(conn, action, args)
    } else if action.starts_with("note_") {
        notes::execute(conn, action, args)
    } else {
        conversation::execute(conn, action, args)
    }
}
