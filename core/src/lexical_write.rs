//! File generation replacement against the existing FTS5 projection.
use rusqlite::{params, types::Value as SqlValue, Connection, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

const INVALID: &str = "LEXICAL_REPLACEMENT_INVALID";
const STORAGE: &str = "LEXICAL_REPLACEMENT_STORAGE";
const MAX_ROWS: usize = 200_000;
const MAX_LINE: u64 = 32 * 1024 * 1024;
const INSERT: &str = "INSERT INTO lexical_chunks
    (collection,point_id,dataset_id,doc_id,doc_name,text,content_hash,chunk_ord,
     section_heading,parent_id,parent_ord,child_ord,parent_heading,context_before,context_after,
     context_kind,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
    ON CONFLICT(collection,point_id) DO UPDATE SET
     doc_id=excluded.doc_id,text=excluded.text,content_hash=excluded.content_hash,
     chunk_ord=excluded.chunk_ord,section_heading=excluded.section_heading,parent_id=excluded.parent_id,
     parent_ord=excluded.parent_ord,child_ord=excluded.child_ord,parent_heading=excluded.parent_heading,
     context_before=excluded.context_before,context_after=excluded.context_after,
     context_kind=excluded.context_kind,updated_at=excluded.updated_at
    WHERE lexical_chunks.dataset_id=excluded.dataset_id AND lexical_chunks.doc_name=excluded.doc_name";

fn required<'a>(row: &'a Value, key: &str) -> Result<&'a str, &'static str> {
    row[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or(INVALID)
}

fn ordinal(value: &Value) -> Result<SqlValue, &'static str> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| value.as_f64().map(SqlValue::Real))
            .ok_or(INVALID),
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        _ => Err(INVALID),
    }
}

fn with_rows(
    request: &Value,
    mut insert: impl FnMut(&Value) -> Result<(), &'static str>,
) -> Result<usize, &'static str> {
    if let Some(rows) = request["rows"].as_array() {
        if rows.is_empty() || rows.len() > MAX_ROWS {
            return Err(INVALID);
        }
        for row in rows {
            insert(row)?;
        }
        return Ok(rows.len());
    }
    let path = Path::new(required(request, "spool")?);
    if !path.is_absolute() || !path.is_file() {
        return Err(INVALID);
    }
    let mut file = BufReader::new(File::open(path).map_err(|_| STORAGE)?);
    let mut line = Vec::new();
    let mut count = 0;
    loop {
        line.clear();
        if (&mut file)
            .take(MAX_LINE + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| STORAGE)?
            == 0
        {
            break;
        }
        count += 1;
        if count > MAX_ROWS || line.len() as u64 > MAX_LINE || !line.ends_with(b"\n") {
            return Err(INVALID);
        }
        insert(&serde_json::from_slice(&line).map_err(|_| INVALID)?)?;
    }
    if count == 0 {
        Err(INVALID)
    } else {
        Ok(count)
    }
}

pub fn replace(request: &Value) -> Result<Value, &'static str> {
    let mut conn = open(request)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| STORAGE)?;
    let count = replace_in(&tx, request)?;
    tx.commit().map_err(|_| STORAGE)?;
    Ok(json!({"replaced":count}))
}

pub(crate) fn open(request: &Value) -> Result<Connection, &'static str> {
    let path = Path::new(required(request, "database")?);
    if !path.is_absolute() {
        return Err(INVALID);
    }
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| STORAGE)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| STORAGE)?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(|_| STORAGE)?;
    Ok(conn)
}

pub(crate) fn replace_in(tx: &Transaction<'_>, request: &Value) -> Result<usize, &'static str> {
    let collection = required(request, "collection")?;
    let dataset = required(request, "dataset_id")?;
    let name = required(request, "doc_name")?;
    tx.execute(
        "DELETE FROM lexical_chunks WHERE collection=?1 AND dataset_id=?2 AND doc_name=?3",
        params![collection, dataset, name],
    )
    .map_err(|_| STORAGE)?;
    let now = chrono::Utc::now().timestamp_micros() as f64 / 1_000_000.0;
    let count = {
        let mut statement = tx.prepare(INSERT).map_err(|_| STORAGE)?;
        with_rows(request, |row| {
            if row["dataset_id"] != dataset || row["doc_name"] != name {
                return Err(INVALID);
            }
            let field = |key| row[key].as_str().unwrap_or_default();
            let changed = statement
                .execute(params![
                    collection,
                    required(row, "point_id")?,
                    dataset,
                    field("doc_id"),
                    name,
                    required(row, "text")?,
                    required(row, "content_hash")?,
                    ordinal(&row["chunk_ord"])?,
                    field("section_heading"),
                    field("parent_id"),
                    ordinal(&row["parent_ord"])?,
                    ordinal(&row["child_ord"])?,
                    field("parent_heading"),
                    field("context_before"),
                    field("context_after"),
                    field("context_kind"),
                    now
                ])
                .map_err(|_| STORAGE)?;
            if changed != 1 {
                return Err("LEXICAL_POINT_SCOPE_CONFLICT");
            }
            Ok(())
        })?
    };
    Ok(count)
}
