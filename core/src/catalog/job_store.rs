//! Durable jobs: one transaction owner, existing schema and native transition policy.
use super::*;
use rusqlite::{params, types::Value as SqlValue};

fn save_patch(conn: &Connection, id: &str, decision: Value) -> Result<()> {
    if decision["ignored"] == true {
        return Ok(());
    }
    for (key, value) in decision["patch"]
        .as_object()
        .ok_or_else(|| Failure::invalid("Invalid job patch"))?
    {
        // Keys come exclusively from the closed native transition policy.
        let sql = match value {
            Value::String(v) => SqlValue::Text(v.clone()),
            Value::Number(v) => SqlValue::Integer(
                v.as_i64()
                    .ok_or_else(|| Failure::invalid("Invalid job counter"))?,
            ),
            Value::Null => SqlValue::Null,
            _ => return Err(Failure::invalid("Invalid job field")),
        };
        conn.execute(
            &format!("UPDATE jobs SET {key}=? WHERE id=?"),
            params![sql, id],
        )?;
    }
    Ok(())
}
// Job result is opaque JSON text across the ABI: never round large integers
// through serde_json::Value. The compatibility codec retains Python JSON semantics.
pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    match action {
        "job_init" => {
            conn.execute_batch("CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,type TEXT NOT NULL,status TEXT NOT NULL,
                source TEXT DEFAULT '',dataset_id TEXT DEFAULT '',dataset_name TEXT DEFAULT '',
                total INTEGER DEFAULT 0,processed INTEGER DEFAULT 0,errors INTEGER DEFAULT 0,
                message TEXT DEFAULT '',result TEXT DEFAULT '{}',started_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,finished_at TEXT DEFAULT NULL);")?;
            Ok(Value::Null)
        }
        "job_get" => Ok(first(conn,"SELECT * FROM jobs WHERE id=?",[text(args,"id")?])?),
        "job_pending_documents" => {
            if !table(conn, "documents")? {
                return Ok(json!(0));
            }
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM documents WHERE upper(status)='PENDING'",
                [],
                |row| row.get(0),
            )?;
            Ok(json!(count))
        }
        "job_list" => {
            let limit=integer(args,"limit",200)?;
            let data=rows(conn,"SELECT * FROM jobs ORDER BY started_at DESC LIMIT ?",[limit])?;
            Ok(Value::Object(data.into_iter().map(|row|(row["id"].as_str().unwrap().to_owned(),row)).collect()))
        }
        "job_active" => Ok(Value::Array(rows(conn,"SELECT id FROM jobs WHERE finished_at IS NULL AND upper(status) IN ('QUEUED','PARSING','RUNNING') ORDER BY updated_at DESC LIMIT ?",[integer(args,"limit",500)?])?.into_iter().map(|r|r["id"].clone()).collect())),
        "job_create" => {
            let id=text(args,"id")?;let now=text(args,"now")?;
            let kind = text(args, "type")?;
            let patch=args.get("patch").ok_or_else(||Failure::invalid("Missing job fields"))?;
            let decision=crate::jobs::patch(&json!({"current":{"status":"queued"},"patch":patch,"now":now})).map_err(Failure::invalid)?;
            let p=&decision["patch"];
            if kind == "rag_parse_scheduler"
                && matches!(p["status"].as_str(), Some("queued" | "running" | "parsing"))
                && conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE type=? AND coalesce(finished_at,'')='' AND lower(status) IN ('queued','running','parsing'))",
                    [kind],
                    |row| row.get::<_, bool>(0),
                )?
            {
                return Ok(Value::Null);
            }
            conn.execute("INSERT INTO jobs(id,type,status,source,dataset_id,dataset_name,total,processed,errors,message,result,started_at,updated_at,finished_at) VALUES (?,?,?,?,?,?,?,0,0,?,'{}',?,?,?)",params![id,kind,p["status"].as_str(),p["source"].as_str(),p["dataset_id"].as_str(),p["dataset_name"].as_str(),p["total"].as_i64(),p["message"].as_str(),now,now,p["finished_at"].as_str()])?;
            Ok(first(conn,"SELECT * FROM jobs WHERE id=?",[id])?)
        }
        "job_update" => {
            let id=text(args,"id")?;
            let current=first(conn,"SELECT * FROM jobs WHERE id=?",[id])?;
            if current.is_null(){return Ok(json!({}));}
            let decision=crate::jobs::patch(&json!({"current":current,"patch":args["patch"],"now":text(args,"now")?})).map_err(Failure::invalid)?;
            save_patch(conn,id,decision)?;
            Ok(first(conn,"SELECT * FROM jobs WHERE id=?",[id])?)
        }
        "job_recover" => {
            let reason=text(args,"reason")?;let now=text(args,"now")?;let mut count=0;
            loop {
                let active=rows(conn,"SELECT * FROM jobs WHERE finished_at IS NULL AND lower(status) IN ('queued','running','parsing') LIMIT 128",[])?;
                if active.is_empty(){break;}
                for row in active {
                    let decision=crate::jobs::recovery(&json!({"current":row,"reason":reason,"now":now})).map_err(Failure::invalid)?;
                    if decision["ignored"]==true{return Err(Failure::invalid("Active job not recoverable"));}
                    save_patch(conn,row["id"].as_str().unwrap(),decision)?;count+=1;
                }
            }
            Ok(json!(count))
        }
        _=>Err(Failure::invalid("Unknown job action")),
    }
}
