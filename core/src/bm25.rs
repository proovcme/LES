//! Transactional postings owner. No source documents, models or journal publication here.
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

type Result<T> = std::result::Result<T, &'static str>;
type Writers = HashMap<u64, Connection>;
fn fuse(r: &Value) -> Result<Value> {
    let constant = r["rank_constant"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.)
        .ok_or("RRF_CONSTANT_INVALID")?;
    let mut positions = HashMap::new();
    let mut rows = Vec::<(Value, f64)>::new();
    for channel in r["channels"].as_array().ok_or("RRF_REQUEST_INVALID")? {
        for (rank, id) in channel
            .as_array()
            .ok_or("RRF_REQUEST_INVALID")?
            .iter()
            .enumerate()
        {
            if !(id.is_string() || id.as_u64().is_some()) {
                return Err("RRF_REQUEST_INVALID");
            }
            let key = id.to_string();
            let index = *positions.entry(key).or_insert_with(|| {
                rows.push((id.clone(), 0.));
                rows.len() - 1
            });
            rows[index].1 += 1. / (constant + rank as f64);
        }
    }
    rows.sort_by(|a, b| b.1.total_cmp(&a.1)); // Stable ties retain first channel's encounter order.
    let limit = r["limit"].as_i64().ok_or("RRF_REQUEST_INVALID")?;
    let count = if limit < 0 {
        rows.len().saturating_sub(limit.unsigned_abs() as usize)
    } else {
        limit as usize
    };
    Ok(
        json!({"hits":rows.into_iter().take(count).map(|(id,score)| json!([id,score])).collect::<Vec<_>>()}),
    )
}
fn writers() -> &'static Mutex<Writers> {
    static WRITERS: OnceLock<Mutex<Writers>> = OnceLock::new();
    WRITERS.get_or_init(|| Mutex::new(HashMap::new()))
}
fn sql<T>(value: rusqlite::Result<T>) -> Result<T> {
    value.map_err(|_| "INDEX_SPARSE_CONTRACT_INVALID")
}
fn cached_execute(
    db: &Connection,
    statement: &str,
    args: impl rusqlite::Params,
) -> rusqlite::Result<usize> {
    db.prepare_cached(statement)?.execute(args)
}
fn string<'a>(r: &'a Value, key: &str) -> Result<&'a str> {
    r[key].as_str().ok_or("INDEX_SPARSE_REQUEST_INVALID")
}
fn open(r: &Value, create: bool) -> Result<Connection> {
    let path = Path::new(string(r, "database")?);
    if !path.is_absolute() {
        return Err("INDEX_SPARSE_REQUEST_INVALID");
    }
    if !create && !path.is_file() {
        return Err("INDEX_SPARSE_STALE");
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | if create {
            OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::empty()
        };
    let db = sql(Connection::open_with_flags(path, flags))?;
    sql(db.busy_timeout(std::time::Duration::from_secs(5)))?;
    sql(db.execute_batch("PRAGMA foreign_keys=ON"))?;
    Ok(db)
}
fn metadata(db: &Connection) -> Result<Value> {
    sql(db.query_row(
        "SELECT n,length,revision FROM corpus WHERE id=1",
        [],
        |row| {
            Ok(json!([
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?
            ]))
        },
    ))
}
fn scalar(value: &Value) -> Result<rusqlite::types::Value> {
    use rusqlite::types::Value as Sql;
    match value {
        Value::Null => Ok(Sql::Null),
        Value::String(v) => Ok(Sql::Text(v.clone())),
        Value::Bool(v) => Ok(Sql::Integer(i64::from(*v))),
        Value::Number(v) => {
            if let Some(v) = v.as_i64() {
                Ok(Sql::Integer(v))
            } else if v.as_u64().is_some() {
                Err("INDEX_SPARSE_REQUEST_INVALID")
            } else {
                v.as_f64()
                    .map(Sql::Real)
                    .ok_or("INDEX_SPARSE_REQUEST_INVALID")
            }
        }
        _ => Err("INDEX_SPARSE_REQUEST_INVALID"),
    }
}
fn remove(db: &Connection, id: &str) -> Result<()> {
    let length: Option<i64> = sql(
        sql(db.prepare_cached("SELECT length FROM docs WHERE id=?"))?
            .query_row([id], |r| r.get(0))
            .optional(),
    )?;
    if let Some(length) = length {
        sql(cached_execute(
            db,
            "UPDATE frequencies SET df=df-1 WHERE term IN (SELECT term FROM postings WHERE doc=?)",
            [id],
        ))?;
        sql(cached_execute(db, "DELETE FROM docs WHERE id=?", [id]))?;
        sql(cached_execute(
            db,
            "UPDATE corpus SET n=n-1,length=length-? WHERE id=1",
            [length],
        ))?;
    }
    Ok(())
}
fn put(db: &Connection, r: &Value) -> Result<Value> {
    let points = r["points"]
        .as_array()
        .ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
    if points.len() > 256 {
        return Err("INDEX_SPARSE_REQUEST_INVALID");
    }
    // Tokenization/Unicode normalization is passed through the versioned lexical boundary.
    for point in points {
        let id = string(point, "id")?; // JSON-encoded Qdrant ID, byte-compatible with the existing schema.
        let terms = point["terms"]
            .as_object()
            .ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
        let mut length = 0i64;
        for tf in terms.values() {
            let tf = tf
                .as_i64()
                .filter(|v| *v > 0)
                .ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
            length = length
                .checked_add(tf)
                .ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
        }
        remove(db, id)?;
        let p = &point["payload"];
        sql(cached_execute(
            db,
            "INSERT INTO docs VALUES (?,?,?,?,?)",
            params![
                id,
                length,
                scalar(&p["dataset_id"])?,
                scalar(&p["file_name"])?,
                scalar(&p["node_role"])?
            ],
        ))?;
        let mut posting = sql(db.prepare_cached("INSERT INTO postings VALUES (?,?,?)"))?;
        let mut frequency = sql(db.prepare_cached(
            "INSERT INTO frequencies VALUES (?,1) ON CONFLICT(term) DO UPDATE SET df=df+1",
        ))?;
        for (term, tf) in terms {
            sql(posting.execute(params![term, id, tf.as_i64().unwrap()]))?;
            sql(frequency.execute([term]))?;
        }
        if !p["ancestor_ids"].is_null() {
            let ancestors = p["ancestor_ids"]
                .as_array()
                .ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
            let mut ancestor =
                sql(db.prepare_cached("INSERT OR IGNORE INTO ancestors VALUES (?,?)"))?;
            for item in ancestors {
                sql(ancestor.execute(params![
                    item.as_str().ok_or("INDEX_SPARSE_REQUEST_INVALID")?,
                    id
                ]))?;
            }
        }
        sql(cached_execute(
            db,
            "UPDATE corpus SET n=n+1,length=length+? WHERE id=1",
            [length],
        ))?;
    }
    Ok(json!({}))
}
fn clear(db: &Connection, scope: &Value) -> Result<Value> {
    if let Some(ids) = scope["ids"].as_array() {
        for id in ids {
            remove(db, id.as_str().ok_or("INDEX_SPARSE_REQUEST_INVALID")?)?;
        }
    } else {
        let mut args = Vec::<rusqlite::types::Value>::new();
        let mut clause = if scope.get("datasets").is_some() {
            args.push(scope["datasets"].to_string().into());
            "dataset IN (SELECT value FROM json_each(?))".to_owned()
        } else {
            args.push(string(scope, "dataset")?.to_owned().into());
            "dataset=?".to_owned()
        };
        if scope.get("file").is_some() {
            clause.push_str(" AND file=?");
            args.push(string(scope, "file")?.to_owned().into());
        }
        loop {
            let mut statement =
                sql(db.prepare(&format!("SELECT id FROM docs WHERE {clause} LIMIT 128")))?;
            let rows =
                sql(sql(statement
                    .query_map(rusqlite::params_from_iter(&args), |r| r.get::<_, String>(0)))?
                .collect::<rusqlite::Result<Vec<_>>>())?;
            if rows.is_empty() {
                break;
            }
            for id in rows {
                remove(db, &id)?;
            }
        }
    }
    Ok(json!({}))
}
fn search(r: &Value) -> Result<Value> {
    let mut db = open(r, false)?;
    let tx = sql(db.transaction())?;
    let meta = metadata(&tx)?;
    if meta[2] != r["revision"] {
        return Err("INDEX_SPARSE_STALE");
    }
    let n = meta[0].as_f64().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
    let length = meta[1].as_f64().ok_or("INDEX_SPARSE_CONTRACT_INVALID")?;
    if n == 0. {
        return Ok(json!({"hits":[]}));
    }
    sql(tx.execute_batch("CREATE TEMP TABLE query_terms(term TEXT PRIMARY KEY,idf REAL)"))?;
    for term in r["terms"]
        .as_array()
        .ok_or("INDEX_SPARSE_REQUEST_INVALID")?
    {
        let term = term.as_str().ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
        let df: Option<i64> = sql(tx
            .query_row("SELECT df FROM frequencies WHERE term=?", [term], |row| {
                row.get(0)
            })
            .optional())?;
        if let Some(df) = df.filter(|v| *v > 0) {
            let df = df as f64;
            let idf = ((n - df + 0.5) / (df + 0.5)).ln_1p();
            if !idf.is_finite() {
                return Err("INDEX_SPARSE_CONTRACT_INVALID");
            }
            sql(tx.execute(
                "INSERT OR IGNORE INTO query_terms VALUES (?,?)",
                params![term, idf],
            ))?;
        }
    }
    let mut clauses = Vec::new();
    let mut args: Vec<rusqlite::types::Value> =
        vec![(if length == 0. { 1. } else { length / n }).into()];
    for (column, key) in [
        ("dataset", "dataset_ids"),
        ("file", "doc_filter"),
        ("role", "node_roles"),
    ] {
        if let Some(values) = r[key].as_array().filter(|v| !v.is_empty()) {
            let predicate = format!("d.{column} IN (SELECT value FROM json_each(?))");
            clauses.push(if key == "node_roles" && values.iter().any(|v| v == "") {
                format!("({predicate} OR d.{column} IS NULL)")
            } else {
                predicate
            });
            args.push(json!(values).to_string().into());
        }
    }
    if let Some(values) = r["ancestor_ids"].as_array().filter(|v| !v.is_empty()) {
        clauses.push("EXISTS (SELECT 1 FROM ancestors a WHERE a.doc=d.id AND a.ancestor IN (SELECT value FROM json_each(?)))".into());
        args.push(json!(values).to_string().into());
    }
    let limit = r["limit"].as_i64().ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
    args.push(limit.into());
    let clause = if clauses.is_empty() {
        "1".into()
    } else {
        clauses.join(" AND ")
    };
    let mut statement = sql(tx.prepare(&format!("SELECT d.id,SUM(q.idf*p.tf*2.2/(p.tf+1.2*(.25+.75*d.length/?))) AS score FROM query_terms q CROSS JOIN postings p ON p.term=q.term JOIN docs d ON d.id=p.doc WHERE {clause} GROUP BY d.id ORDER BY score DESC,d.id LIMIT ?")))?;
    let hits = sql(sql(
        statement.query_map(rusqlite::params_from_iter(args), |row| {
            Ok(json!([row.get::<_, String>(0)?, row.get::<_, f64>(1)?]))
        }),
    )?
    .collect::<rusqlite::Result<Vec<_>>>())?;
    Ok(json!({"hits":hits}))
}

pub fn execute(r: &Value) -> Result<Value> {
    match string(r, "action")? {
        "fuse" => fuse(r),
        "metadata" => Ok(json!({"metadata":metadata(&open(r,false)?)?})),
        "search" => search(r),
        "begin" => {
            let db = open(r, true)?;
            sql(db.execute_batch(include_str!("bm25_schema.sql")))?;
            sql(db.execute_batch("BEGIN IMMEDIATE"))?;
            let mut handles = writers().lock().map_err(|_| "INDEX_SPARSE_OWNER_FAILED")?;
            if handles.len() >= 8 {
                return Err("INDEX_SPARSE_OWNER_BUSY");
            }
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let handle = NEXT.fetch_add(1, Ordering::Relaxed);
            handles.insert(handle, db);
            Ok(json!({"handle":handle}))
        }
        action => {
            let handle = r["handle"].as_u64().ok_or("INDEX_SPARSE_REQUEST_INVALID")?;
            let mut handles = writers().lock().map_err(|_| "INDEX_SPARSE_OWNER_FAILED")?;
            if action == "rollback" {
                handles.remove(&handle); // SQLite rolls back on drop, including failed commits.
                return Ok(json!({}));
            }
            let db = handles.get(&handle).ok_or("INDEX_SPARSE_HANDLE_INVALID")?;
            let result = match action {
                "reset" => sql(db.execute_batch("DELETE FROM docs;DELETE FROM frequencies;UPDATE corpus SET n=0,length=0,revision='' WHERE id=1")).map(|_| json!({})),
                "clear" => clear(db,&r["scope"]),
                "put" => put(db,r),
                "commit" => (|| {
                    sql(cached_execute(db,"DELETE FROM frequencies WHERE df=0", []))?;
                    sql(cached_execute(db,"UPDATE corpus SET revision=? WHERE id=1", [string(r,"revision")?]))?;
                    let meta = metadata(db)?;
                    if meta[0] != r["expected_count"] { return Err("INDEX_SPARSE_STALE"); }
                    sql(db.execute_batch("COMMIT"))?;
                    Ok(json!({"metadata":meta}))
                })(),
                _ => Err("INDEX_SPARSE_REQUEST_INVALID"),
            };
            // A failed batch invalidates the whole writer, never a partially usable transaction.
            if result.is_err() || action == "commit" {
                handles.remove(&handle);
            }
            result
        }
    }
}
