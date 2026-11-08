//! Replay-safe import. A cursor is durable only after Edge flush; replay preserves point IDs.
use crate::{Result, Shard, database, digest};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
const MAX_LINE: u64 = 8 * 1024 * 1024;
fn line(reader: &mut BufReader<File>) -> Result<Option<Value>> {
    let mut bytes = Vec::new();
    let n = reader
        .take(MAX_LINE + 1)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?;
    if n == 0 {
        return Ok(None);
    }
    if n as u64 > MAX_LINE || !bytes.ends_with(b"\n") {
        return Err("EDGE_SOURCE_RECORD_INVALID");
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "EDGE_SOURCE_RECORD_INVALID")
}
fn source(manifest: &Value) -> Result<PathBuf> {
    if manifest["schema"] != "les.edge-import.v1"
        || !manifest["collection"]
            .as_str()
            .is_some_and(|v| !v.is_empty() && v.len() <= 255)
        || !manifest["source_sha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        || manifest["points"].as_u64().is_none()
    {
        return Err("EDGE_IMPORT_MANIFEST_INVALID");
    }
    let path = PathBuf::from(
        manifest["source"]
            .as_str()
            .ok_or("EDGE_IMPORT_MANIFEST_INVALID")?,
    );
    if !path.is_absolute() || !path.is_file() {
        return Err("EDGE_SOURCE_UNAVAILABLE");
    }
    if digest(&path)? != manifest["source_sha256"] {
        return Err("EDGE_MIGRATION_SOURCE_CHANGED");
    }
    Ok(path)
}
fn close_numbers(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
                return a == b;
            }
            if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
                return a == b;
            }
            match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => {
                    a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-6f64.max(a.abs() * 1e-6)
                }
                _ => false,
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| close_numbers(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, a)| b.get(k).is_some_and(|b| close_numbers(a, b)))
        }
        _ => a == b,
    }
}
fn verify(shard: &Shard, path: &Path, expected: u64) -> Result<Value> {
    if shard.call("count", &json!({"exact":true}))?["count"].as_u64() != Some(expected) {
        return Err("EDGE_POINT_COUNT_MISMATCH");
    }
    let mut input = BufReader::new(File::open(path).map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?);
    let mut count = 0u64;
    loop {
        let mut batch = Vec::new();
        for _ in 0..64 {
            if let Some(row) = line(&mut input)? {
                batch.push(row);
            } else {
                break;
            }
        }
        if batch.is_empty() {
            break;
        }
        let rows=shard.call("retrieve",&json!({"ids":batch.iter().map(|p|&p["id"]).collect::<Vec<_>>(),"with_payload":true,"with_vector":true}))?;
        let rows = rows.as_array().ok_or("EDGE_VERIFY_FAILED")?;
        if rows.len() != batch.len() {
            return Err("EDGE_VERIFY_FAILED");
        }
        for (a, b) in batch.iter().zip(rows) {
            if a["id"] != b["id"]
                || a["payload"] != b["payload"]
                || !close_numbers(&a["vector"], &b["vector"])
            {
                return Err("EDGE_VERIFY_FAILED");
            }
        }
        count += batch.len() as u64;
    }
    if count != expected
        || shard.call("count", &json!({"exact":true}))?["count"].as_u64() != Some(expected)
    {
        return Err("EDGE_POINT_COUNT_MISMATCH");
    }
    Ok(json!({"points":count,"payloads_equal":true,"ids_equal":true,"vectors_tolerance":1e-6}))
}
pub fn import(
    manifest_path: &Path,
    target: &Path,
    mut progress: impl FnMut(Value),
) -> Result<Value> {
    let file = File::open(manifest_path).map_err(|_| "EDGE_IMPORT_MANIFEST_INVALID")?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "EDGE_IMPORT_MANIFEST_INVALID")?;
    if bytes.len() > 1024 * 1024 {
        return Err("EDGE_IMPORT_MANIFEST_INVALID");
    }
    let manifest: Value =
        serde_json::from_slice(&bytes).map_err(|_| "EDGE_IMPORT_MANIFEST_INVALID")?;
    let path = source(&manifest)?;
    let shard = Shard::load(target, Some(&manifest), false)?;
    let result = (|| {
        let db = database(&shard.root)?;
        let (phase, schema) = db
            .query_row(
                "SELECT phase,native_schema FROM migration WHERE id=1",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
        if phase == "ready" && schema >= 2 {
            return Ok(json!({"status":"already_migrated","info":shard.info()?}));
        }

        let (cursor, mut count) = db
            .query_row(
                "SELECT next_byte,points FROM migration WHERE id=1",
                [],
                |r| {
                    Ok((
                        u64::try_from(r.get::<_, i64>(0)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        u64::try_from(r.get::<_, i64>(1)?)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    ))
                },
            )
            .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
        let expected = manifest["points"].as_u64().unwrap();
        progress(
            json!({"phase":"importing","completed":count,"total":expected,"resumed":cursor>0}),
        );
        let mut input = BufReader::new(File::open(&path).map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?);
        if cursor
            > input
                .get_ref()
                .metadata()
                .map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?
                .len()
        {
            return Err("EDGE_CATALOG_INVALID");
        }
        input
            .seek(SeekFrom::Start(cursor))
            .map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?;
        loop {
            let mut batch = Vec::new();
            for _ in 0..64 {
                if let Some(row) = line(&mut input)? {
                    batch.push(row);
                } else {
                    break;
                }
            }
            if batch.is_empty() {
                break;
            }
            if count + batch.len() as u64 > expected {
                return Err("EDGE_POINT_COUNT_MISMATCH");
            }
            shard.upsert(&batch)?; // Flush precedes cursor: an interrupted batch can be replayed.
            count += batch.len() as u64;
            let cursor = input
                .stream_position()
                .map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?;
            db.execute("UPDATE migration SET phase='importing',next_byte=?1,points=?2,error_code=NULL WHERE id=1",rusqlite::params![i64::try_from(cursor).map_err(|_|"EDGE_SOURCE_TOO_LARGE")?,i64::try_from(count).map_err(|_|"EDGE_SOURCE_TOO_LARGE")?]).map_err(|_|"EDGE_CATALOG_UNAVAILABLE")?;
            progress(json!({"phase":"importing","completed":count,"total":expected}));
        }
        if count != expected {
            return Err("EDGE_POINT_COUNT_MISMATCH");
        }
        db.execute(
            "UPDATE migration SET phase='verifying',error_code=NULL WHERE id=1",
            [],
        )
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
        progress(json!({"phase":"verifying","completed":count,"total":expected}));
        let proof = verify(&shard, &path, expected)?;
        db.execute("UPDATE migration SET phase='lexical' WHERE id=1", [])
            .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
        shard.refresh_postings(true, |value| progress(value))?;

        if digest(&path)? != manifest["source_sha256"] {
            return Err("EDGE_MIGRATION_SOURCE_CHANGED");
        }
        db.execute(
            "UPDATE migration SET phase='ready',error_code=NULL,native_schema=2 WHERE id=1",
            [],
        )
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
        progress(json!({"phase":"ready","completed":count,"total":expected}));
        Ok(json!({"status":"ready","proof":proof}))
    })();
    if let Err(code) = result {
        if let Ok(db) = database(&shard.root) {
            let _ = db.execute(
                "UPDATE migration SET phase='failed',error_code=?1 WHERE id=1",
                [code],
            );
        }
    }
    result
}
