//! Durable commit decisions and sparse invalidations on the existing journal.
use crate::workspace::{connection, Failure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{collections::HashSet, path::Path};

const INCOMPLETE: &str = "INDEX_RECOVERY_INCOMPLETE";
const INVALID: &str = "INDEX_JOURNAL_INVALID";

fn db_error(error: rusqlite::Error) -> &'static str {
    workspace_error(Failure::from(error))
}

fn workspace_error(error: Failure) -> &'static str {
    if error.code == "WORKSPACE_BUSY" {
        "INDEX_UPDATE_BUSY"
    } else {
        INCOMPLETE
    }
}

fn table(conn: &Connection, name: &str) -> Result<bool, &'static str> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |row| row.get(0),
    )
    .map_err(db_error)
}

fn read(conn: &Connection) -> Result<Value, &'static str> {
    if !table(conn, "replacement")? {
        return Ok(Value::Null);
    }
    let body: Option<String> = conn
        .query_row("SELECT body FROM replacement WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(db_error)?;
    let Some(body) = body else {
        return Ok(Value::Null);
    };
    let value: Value = serde_json::from_str(&body).map_err(|_| INCOMPLETE)?;
    if !matches!(
        value["phase"].as_str(),
        Some("idle" | "staging" | "committing" | "sparse_dirty" | "sparse_refresh")
    ) {
        return Err(INCOMPLETE);
    }
    Ok(value)
}

fn store(conn: &Connection, value: &Value) -> Result<(), &'static str> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS replacement (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
    )
    .map_err(db_error)?;
    conn.execute(
        "INSERT OR REPLACE INTO replacement VALUES (1,?)",
        [value.to_string()],
    )
    .map_err(db_error)?;
    Ok(())
}

fn sparse_table(conn: &Connection) -> Result<(), &'static str> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS sparse_changes (scope TEXT PRIMARY KEY)")
        .map_err(db_error)
}

fn changes(conn: &Connection) -> Result<Value, &'static str> {
    if !table(conn, "sparse_changes")? {
        return Ok(json!([]));
    }
    let mut statement = conn
        .prepare("SELECT scope FROM sparse_changes")
        .map_err(db_error)?;
    let mut rows = statement.query([]).map_err(db_error)?;
    let mut unique = HashSet::new();
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(db_error)? {
        let raw: String = row.get(0).map_err(db_error)?;
        let scope: Value = serde_json::from_str(&raw).map_err(|_| INCOMPLETE)?;
        if unique.insert(scope.to_string()) {
            result.push(scope);
        }
    }
    Ok(json!(result))
}

fn revision() -> Result<String, &'static str> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| "INDEX_JOURNAL_RANDOM_UNAVAILABLE")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write(conn: &Connection, action: &str, args: &Value) -> Result<Value, &'static str> {
    let mut current = read(conn)?;
    let phase = current["phase"].as_str().unwrap_or("idle");
    match action {
        "begin" => {
            if phase != "idle" {
                return Err("INDEX_RECOVERY_REQUIRED");
            }
            let ids = args["ids"]
                .as_array()
                .filter(|ids| !ids.is_empty())
                .ok_or(INVALID)?;
            let mut unique = HashSet::new();
            if !ids.iter().all(|id| {
                id.as_str()
                    .is_some_and(|id| !id.is_empty() && unique.insert(id))
            }) {
                return Err(INVALID);
            }
            store(
                conn,
                &json!({"phase":"staging","revision":revision()?,"ids":ids,
                "dataset":args["dataset"].as_str().ok_or(INVALID)?,
                "file_name":args["file_name"].as_str().ok_or(INVALID)?,
                "db_file_name":args["db_file_name"].as_str().ok_or(INVALID)?}),
            )?;
        }
        "commit" => {
            if phase != "staging" {
                return Err("INDEX_RECOVERY_REQUIRED");
            }
            current["phase"] = "committing".into();
            store(conn, &current)?;
        }
        "finish" => {
            if current.is_null() {
                return Ok(Value::Null);
            }
            let scope = if matches!(phase, "staging" | "committing") {
                json!({"dataset":current["dataset"].as_str().ok_or(INCOMPLETE)?,
                       "file":current["file_name"].as_str().ok_or(INCOMPLETE)?})
            } else {
                current["scope"].clone()
            };
            sparse_table(conn)?;
            conn.execute(
                "INSERT OR IGNORE INTO sparse_changes VALUES (?)",
                [scope.to_string()],
            )
            .map_err(db_error)?;
            store(conn, &json!({"phase":"idle","revision":revision()?}))?;
        }
        "store_sparse" => {
            let value = &args["value"];
            let next = value["phase"].as_str().ok_or(INVALID)?;
            if !matches!(next, "idle" | "sparse_dirty" | "sparse_refresh")
                || !value["revision"].is_string()
            {
                return Err(INVALID);
            }
            if matches!(phase, "staging" | "committing")
                || next == "sparse_dirty" && phase != "idle"
            {
                return Err("INDEX_RECOVERY_REQUIRED");
            }
            store(conn, value)?;
        }
        "publish_sparse" => {
            if !matches!(phase, "sparse_refresh" | "sparse_dirty") {
                return Err("INDEX_RECOVERY_REQUIRED");
            }
            sparse_table(conn)?;
            conn.execute("DELETE FROM sparse_changes", [])
                .map_err(db_error)?;
            store(
                conn,
                &json!({"phase":"idle","revision":args["revision"].as_str().ok_or(INVALID)?}),
            )?;
        }
        _ => return Err(INVALID),
    }
    Ok(Value::Null)
}

fn dispatch(request: &Value) -> Result<Value, &'static str> {
    let path = Path::new(request["database"].as_str().ok_or(INVALID)?);
    if !path.is_absolute() {
        return Err(INVALID);
    }
    let action = request["action"].as_str().ok_or(INVALID)?;
    if matches!(action, "read" | "sparse_changes") {
        let conn = connection(path, false).map_err(workspace_error)?;
        return match (action, conn) {
            ("read", Some(conn)) => read(&conn),
            ("read", None) => Ok(Value::Null),
            (_, Some(conn)) => changes(&conn),
            (_, None) => Ok(json!([])),
        };
    }
    if action == "finish" && !path.try_exists().map_err(|_| INCOMPLETE)? {
        return Ok(Value::Null);
    }
    std::fs::create_dir_all(path.parent().ok_or(INVALID)?).map_err(|_| INCOMPLETE)?;
    let mut conn = connection(path, true)
        .map_err(workspace_error)?
        .ok_or(INCOMPLETE)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    let result = write(&tx, action, &request["arguments"])?;
    tx.commit().map_err(db_error)?;
    Ok(result)
}

pub fn execute(request: &Value) -> Result<Value, &'static str> {
    dispatch(request).map(|value| json!({"value":value}))
}
