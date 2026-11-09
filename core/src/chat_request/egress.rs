//! Default-deny provenance for material included in a cloud request.
use super::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::path::Path;

fn read(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "CLOUD_SCOPE_UNVERIFIED")?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|_| "CLOUD_SCOPE_UNVERIFIED")?;
    Ok(db)
}
pub fn datasets(path: &Path, ids: &Value) -> Result<()> {
    let ids = ids.as_array().ok_or("CLOUD_SCOPE_UNVERIFIED")?;
    if ids.is_empty() {
        return Ok(());
    }
    let mut db = read(path)?;
    let tx = db.transaction().map_err(|_| "CLOUD_SCOPE_UNVERIFIED")?;
    let mut query = tx
        .prepare("SELECT sensitivity FROM datasets WHERE id=?")
        .map_err(|_| "CLOUD_SCOPE_UNVERIFIED")?;
    for id in ids {
        let id = id
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or("CLOUD_SCOPE_UNVERIFIED")?;
        if crate::catalog::mail::private_dataset(&tx, id).map_err(|_| "CLOUD_SCOPE_UNVERIFIED")? {
            return Err("CLOUD_SOURCE_LOCAL_ONLY");
        }
        let value = query
            .query_row([id], |r| r.get::<_, Option<String>>(0))
            .optional()
            .map_err(|_| "CLOUD_SCOPE_UNVERIFIED")?
            .flatten();
        if !value.is_some_and(|v| matches!(v.trim().to_ascii_uppercase().as_str(), "P1" | "P2")) {
            return Err("CLOUD_SOURCE_LOCAL_ONLY");
        }
    }
    Ok(())
}

pub fn history(path: &Path, sid: &str, rows: &Value, tag: &str) -> Result<()> {
    let rows = rows.as_array().ok_or("CLOUD_HISTORY_UNVERIFIED")?;
    if rows.is_empty() {
        return Ok(());
    }
    let db = read(path)?;
    let mut query = db
        .prepare(
            "SELECT h.artifact_json FROM chat_history h
        JOIN les_chat_runs r ON r.history_id=h.id AND r.status='completed'
        WHERE h.id=? AND h.session_id=? AND h.success=1",
        )
        .map_err(|_| "CLOUD_HISTORY_UNVERIFIED")?;
    for row in rows {
        let id = row["id"].as_i64().ok_or("CLOUD_HISTORY_UNVERIFIED")?;
        let artifact = query
            .query_row(params![id, sid], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|_| "CLOUD_HISTORY_UNVERIFIED")?
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .ok_or("CLOUD_HISTORY_UNVERIFIED")?;
        if artifact["egress_tag"] != tag {
            return Err("CLOUD_HISTORY_UNVERIFIED");
        }
    }
    Ok(())
}
