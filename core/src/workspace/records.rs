use super::{Failure, Result};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

pub(super) fn string<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::invalid(&format!("Invalid {name}")))
}
pub(super) fn project(value: Option<&Value>) -> Result<Option<i64>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .filter(|id| *id > 0)
            .map(Some)
            .ok_or_else(|| Failure::invalid("Invalid project_id")),
    }
}
pub(super) fn empty_scope() -> Value {
    json!({"scope_type":"none","project_ids":[],"dataset_ids":[],"selected_sources_only":false})
}
pub(super) fn has_table(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}
fn record(row: &rusqlite::Row) -> rusqlite::Result<Value> {
    let scope: String = row.get(4)?;
    let scope: Value = serde_json::from_str(&scope).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(
        json!({"session_id":row.get::<_,String>(0)?,"project_id":row.get::<_,Option<i64>>(1)?,
        "title":row.get::<_,String>(2)?,"role":row.get::<_,String>(3)?,"scope":scope,
        "created_at":row.get::<_,String>(5)?,"updated_at":row.get::<_,String>(6)?,"registered":true}),
    )
}
fn legacy(conn: &Connection, session_id: Option<&str>) -> Result<Vec<Value>> {
    if !has_table(conn, "chat_history")? {
        return Ok(Vec::new());
    }
    let columns = conn
        .prepare("PRAGMA table_info(chat_history)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !["session_id", "timestamp", "question", "id"]
        .iter()
        .all(|name| columns.iter().any(|column| column == name))
    {
        return Ok(Vec::new());
    }
    let condition = if session_id.is_some() {
        "AND h.session_id=?1"
    } else {
        ""
    };
    let excluded = if has_table(conn, "les_chat_sessions")? {
        "AND NOT EXISTS (SELECT 1 FROM les_chat_sessions s WHERE s.session_id=h.session_id)"
    } else {
        ""
    };
    let sql=format!("SELECT h.session_id, MIN(h.timestamp), MAX(h.timestamp),
        (SELECT substr(first.question,1,200) FROM chat_history first WHERE first.session_id=h.session_id ORDER BY first.id LIMIT 1)
        FROM chat_history h WHERE h.session_id IS NOT NULL AND h.session_id != '' {condition} {excluded}
        GROUP BY h.session_id ORDER BY MAX(h.timestamp) DESC LIMIT 500");
    let mut statement = conn.prepare(&sql)?;
    let arguments: Vec<&dyn rusqlite::ToSql> = session_id
        .as_ref()
        .map(|value| vec![value as &dyn rusqlite::ToSql])
        .unwrap_or_default();
    let values=statement.query_map(arguments.as_slice(),|row|Ok(json!({
        "session_id":row.get::<_,String>(0)?,"created_at":row.get::<_,Option<String>>(1)?,
        "updated_at":row.get::<_,Option<String>>(2)?,"title":row.get::<_,Option<String>>(3)?,
        "project_id":null,"role":"agent","scope":empty_scope(),"registered":false,
    })))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}
pub(crate) fn get(conn: &Connection, session_id: &str) -> Result<Option<Value>> {
    if has_table(conn, "les_chat_sessions")? {
        if let Some(value)=conn.query_row("SELECT session_id,project_id,title,role,scope_json,created_at,updated_at FROM les_chat_sessions WHERE session_id=?",[session_id],record).optional()? {return Ok(Some(value));}
    }
    Ok(legacy(conn, Some(session_id))?.into_iter().next())
}
fn timestamp(value: &Value) -> i64 {
    let Some(text) = value.as_str() else {
        return -62_135_596_800_000_000;
    };
    let normalized = if text.chars().count() > 10 {
        let mut chars: Vec<char> = text.chars().collect();
        chars[10] = 'T';
        chars.into_iter().collect::<String>()
    } else {
        text.to_string()
    };
    if let Ok(date) = DateTime::parse_from_rfc3339(&normalized) {
        return date.timestamp_micros();
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(date) = NaiveDateTime::parse_from_str(text, format) {
            return date.and_utc().timestamp_micros();
        }
    }
    for format in ["%Y-%m-%d", "%Y%m%d", "%G-W%V-%u"] {
        if let Ok(date) = NaiveDate::parse_from_str(text, format) {
            return date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_micros();
        }
    }
    -62_135_596_800_000_000
}
pub(super) fn list(conn: &Connection, project_id: Option<i64>) -> Result<Value> {
    let mut sessions = Vec::new();
    if has_table(conn, "les_chat_sessions")? {
        sessions=conn.prepare("SELECT session_id,project_id,title,role,scope_json,created_at,updated_at FROM les_chat_sessions WHERE project_id IS ? ORDER BY updated_at DESC LIMIT 500")?
            .query_map([project_id],record)?.collect::<rusqlite::Result<Vec<_>>>()?;
    }
    if project_id.is_none() {
        sessions.extend(legacy(conn, None)?);
    }
    sessions.sort_by_key(|row| std::cmp::Reverse(timestamp(&row["updated_at"])));
    sessions.truncate(500);
    Ok(json!(sessions))
}
pub(super) fn draft(conn: &Connection, session_id: &str) -> Result<Value> {
    if get(conn, session_id)?.is_none() {
        return Err(Failure::missing(
            "Диалог не найден. Откройте список диалогов.",
        ));
    }
    if has_table(conn, "les_chat_drafts")? {
        if let Some((revision, content)) = conn
            .query_row(
                "SELECT revision,content_json FROM les_chat_drafts WHERE session_id=?",
                [session_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
        {
            let mut value: Value = serde_json::from_str(&content).map_err(|_| {
                Failure::new(
                    "WORKSPACE_UNAVAILABLE",
                    "Не удалось прочитать сохранённый черновик. Откройте диагностику.",
                )
            })?;
            value
                .as_object_mut()
                .ok_or_else(|| {
                    Failure::new(
                        "WORKSPACE_UNAVAILABLE",
                        "Не удалось прочитать сохранённый черновик.",
                    )
                })?
                .insert("revision".into(), revision.into());
            return Ok(value);
        }
    }
    Ok(json!({"revision":0,"text":"","attachment":null}))
}
pub(super) fn insert(conn: &Connection, session: &Value) -> Result<()> {
    conn.execute(
        "INSERT INTO les_chat_sessions VALUES (?,?,?,?,?,?,?)",
        params![
            session["session_id"].as_str(),
            session["project_id"].as_i64(),
            session["title"].as_str(),
            session["role"].as_str(),
            session["scope"].to_string(),
            session["created_at"].as_str(),
            session["updated_at"].as_str()
        ],
    )?;
    Ok(())
}
pub(super) fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
