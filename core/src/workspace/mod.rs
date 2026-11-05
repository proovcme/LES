//! One SQLite owner for sessions/drafts. Reads never migrate or create a database.
pub(crate) mod records;
mod writes;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub detail: String,
    pub sqlite_extended: Option<i32>,
}
impl Failure {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            sqlite_extended: None,
        }
    }
    pub(crate) fn invalid(detail: &str) -> Self {
        Self::new("WORKSPACE_INVALID", detail)
    }
    pub(crate) fn missing(detail: &str) -> Self {
        Self::new("WORKSPACE_NOT_FOUND", detail)
    }
    pub(crate) fn conflict(detail: &str) -> Self {
        Self::new("WORKSPACE_CONFLICT", detail)
    }
}
impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        let code = if matches!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        ) {
            "WORKSPACE_BUSY"
        } else {
            "WORKSPACE_UNAVAILABLE"
        };
        let mut failure=Self::new(code,"Не удалось записать или прочитать диалог: база временно занята или недоступна. Текст остаётся в этом окне; повторите действие.");
        failure.sqlite_extended = error.sqlite_error().map(|error| error.extended_code);
        failure
    }
}
pub(crate) type Result<T> = std::result::Result<T, Failure>;

pub(crate) fn connection(path: &Path, write: bool) -> Result<Option<Connection>> {
    if !write {
        match path.try_exists() {
            Ok(false) => return Ok(None),
            Ok(true) => {}
            Err(_) => {
                return Err(Failure::new(
                    "WORKSPACE_UNAVAILABLE",
                    "Не удалось проверить доступ к базе диалогов. Проверьте папку данных Леса.",
                ))
            }
        }
    }
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    if write {
        conn.pragma_update(None, "synchronous", "FULL")?;
    }
    Ok(Some(conn))
}

pub fn execute(request: &Value) -> Value {
    match dispatch(request) {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(error) => {
            json!({"status":"failed","code":error.code,"detail":error.detail,"sqlite_extended":error.sqlite_extended})
        }
    }
}

fn dispatch(request: &Value) -> Result<Value> {
    if request["schema"] != "les.workspace.v1" {
        return Err(Failure::invalid("Invalid workspace schema"));
    }
    let path = Path::new(
        request["database"]
            .as_str()
            .ok_or_else(|| Failure::invalid("Invalid database path"))?,
    );
    if !path.is_absolute() {
        return Err(Failure::invalid("Database path must be absolute"));
    }
    let action = request["action"]
        .as_str()
        .ok_or_else(|| Failure::invalid("Invalid workspace action"))?;
    let args = request["arguments"]
        .as_object()
        .ok_or_else(|| Failure::invalid("Invalid workspace arguments"))?;
    if matches!(action, "get" | "list" | "draft_get") {
        let conn = connection(path, false)?;
        return match action {
            "get" => Ok(conn
                .as_ref()
                .map(|conn| records::get(conn, records::string(args, "session_id")?))
                .transpose()?
                .flatten()
                .unwrap_or(Value::Null)),
            "list" => Ok(conn
                .as_ref()
                .map(|conn| records::list(conn, records::project(args.get("project_id"))?))
                .transpose()?
                .unwrap_or_else(|| json!([]))),
            _ => conn
                .as_ref()
                .ok_or_else(|| Failure::missing("Диалог не найден. Откройте список диалогов."))
                .and_then(|conn| records::draft(conn, records::string(args, "session_id")?)),
        };
    }
    if !matches!(action, "create" | "update" | "draft_save") {
        return Err(Failure::invalid("Invalid workspace action"));
    }
    // Validate before opening a write connection, then repeat identity/CAS under the lease.
    writes::validate(action, args)?;
    let mut conn = connection(path, true)?.unwrap();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS les_chat_sessions (
        session_id TEXT PRIMARY KEY, project_id INTEGER, title TEXT NOT NULL, role TEXT NOT NULL,
        scope_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_chat_sessions_project ON les_chat_sessions(project_id,updated_at);")?;
    let result = match action {
        "create" => writes::create(&tx, args)?,
        "update" => writes::update(&tx, args)?,
        _ => writes::save_draft(&tx, args)?,
    };
    tx.commit()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_reads_never_create_a_catalog() {
        let path = std::env::temp_dir().join(format!(
            "les-absent-workspace-{}-{}.sqlite",
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        ));
        for (action, args) in [
            ("get", json!({"session_id":"missing"})),
            ("list", json!({})),
            ("draft_get", json!({"session_id":"missing"})),
        ] {
            let result = execute(
                &json!({"schema":"les.workspace.v1","database":path,"action":action,"arguments":args}),
            );
            if action == "draft_get" {
                assert_eq!(result["code"], "WORKSPACE_NOT_FOUND");
            } else {
                assert_eq!(result["status"], "completed");
            }
            assert!(!path.exists());
        }
    }
}
