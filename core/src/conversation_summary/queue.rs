//! A summary and its queue receipt commit together; recovery never replays a confirmed batch.
use super::Result;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::path::Path;
pub fn pending(path: &Path) -> Result<bool> {
    if !path
        .try_exists()
        .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?
    {
        return Ok(false);
    }
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
    let table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='les_summary_jobs'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?
        .is_some();
    if !table {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM les_summary_jobs WHERE status IN ('pending','running'))",
        [],
        |r| r.get(0),
    )
    .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")
}
fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
    conn.execute_batch("PRAGMA synchronous=FULL;")
        .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
    ensure(&conn)?;
    Ok(conn)
}
fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS les_summary_jobs(
        session_id TEXT PRIMARY KEY,status TEXT NOT NULL,claim_token TEXT NOT NULL DEFAULT '',
        rerun INTEGER NOT NULL DEFAULT 0,attempts INTEGER NOT NULL DEFAULT 0,last_code TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
    Ok(())
}
/// Reused by history's transaction: a confirmed answer cannot lose its outbox.
pub(crate) fn enqueue(conn: &Connection, session: &str) -> Result<()> {
    sid(&json!({"session_id":session}))?;
    ensure(conn)?;
    conn.execute("INSERT INTO les_summary_jobs(session_id,status) VALUES(?,'pending') ON CONFLICT(session_id) DO UPDATE SET status=CASE WHEN status='running' THEN 'running' ELSE 'pending' END,rerun=CASE WHEN status='running' THEN 1 ELSE 0 END,updated_at=CURRENT_TIMESTAMP",[session]).map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> std::path::PathBuf {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).unwrap();
        std::env::temp_dir().join(format!("les-summary-{}.db", crate::sha256_hex(&random)))
    }
    #[test]
    fn crash_and_stale_claim_cannot_overwrite_committed_memory() {
        let path = database();
        assert!(!pending(&path).unwrap());
        assert!(!path.exists());
        execute(&path, "enqueue", &json!({"session_id":"one"})).unwrap();
        let old = execute(&path, "claim", &json!({})).unwrap();
        execute(&path, "recover", &json!({})).unwrap();
        let new = execute(&path, "claim", &json!({})).unwrap();
        assert_ne!(new["claim_token"], old["claim_token"]);
        let args = |claim: &Value, revision: i64, text: &str| json!({"session_id":"one","claim_token":claim["claim_token"],"expected_revision":revision,"changes":{"summary":text,"through_id":6}});
        assert_eq!(
            execute(&path, "commit", &args(&old, 0, "stale")).unwrap_err(),
            "SUMMARY_CLAIM_CHANGED"
        );
        let state = execute(&path, "commit", &args(&new, 0, "original")).unwrap();
        assert_eq!(state["revision"], 1);
        execute(&path, "enqueue", &json!({"session_id":"one"})).unwrap();
        let claim = execute(&path, "claim", &json!({})).unwrap();
        assert_eq!(
            execute(&path, "commit", &args(&claim, 0, "overwrite")).unwrap_err(),
            "SUMMARY_CONTEXT_CHANGED"
        );
        let conn = Connection::open(&path).unwrap();
        let raw: String = conn
            .query_row("SELECT state_json FROM les_conversation_context", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&raw).unwrap()["summary"],
            "original"
        );
        drop(conn);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn enqueue_during_running_is_not_lost() {
        let path = database();
        execute(&path, "enqueue", &json!({"session_id":"one"})).unwrap();
        let mut claim = execute(&path, "claim", &json!({})).unwrap();
        execute(&path, "enqueue", &json!({"session_id":"one"})).unwrap();
        claim["retry"] = json!(false);
        execute(&path, "finish", &claim).unwrap();
        assert_eq!(
            execute(&path, "status", &json!({"session_id":"one"})).unwrap()["status"],
            "pending"
        );
        std::fs::remove_file(path).unwrap();
    }
}
fn sid(args: &Value) -> Result<&str> {
    args["session_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .ok_or("SUMMARY_SESSION_INVALID")
}
pub fn execute(path: &Path, action: &str, args: &Value) -> Result<Value> {
    let mut conn = open(path)?;
    match action {
        "recover" => {
            conn.execute("UPDATE les_summary_jobs SET status='pending',claim_token='',last_code='SUMMARY_INTERRUPTED' WHERE status='running'",[]).map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
            Ok(Value::Null)
        }
        "enqueue" => {
            enqueue(&conn, sid(args)?)?;
            Ok(Value::Null)
        }
        "claim" => {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
            let session=tx.query_row("SELECT session_id FROM les_summary_jobs WHERE status='pending' ORDER BY updated_at,session_id LIMIT 1",[],|r|r.get::<_,String>(0)).optional().map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
            let Some(session) = session else {
                return Ok(Value::Null);
            };
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
            let token = crate::sha256_hex(&random);
            tx.execute("UPDATE les_summary_jobs SET status='running',claim_token=?,rerun=0,attempts=attempts+1,updated_at=CURRENT_TIMESTAMP WHERE session_id=?",params![token,session]).map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
            tx.commit().map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
            Ok(json!({"session_id":session,"claim_token":token}))
        }
        "finish" | "commit" => {
            let session = sid(args)?;
            let token = args["claim_token"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("SUMMARY_CLAIM_INVALID")?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
            let rerun=tx.query_row("SELECT rerun FROM les_summary_jobs WHERE session_id=? AND claim_token=? AND status='running'",params![session,token],|r|r.get::<_,i64>(0)).optional().map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?.ok_or("SUMMARY_CLAIM_CHANGED")?;
            let value = if action == "commit" {
                crate::catalog::update_conversation(&tx,&json!({"session_id":session,"expected_revision":args["expected_revision"],"changes":args["changes"]})).map_err(|e|if e.code=="WORKSPACE_CONFLICT"{"SUMMARY_CONTEXT_CHANGED"}else{"SUMMARY_STORAGE_UNAVAILABLE"})?
            } else {
                Value::Null
            };
            let code = args["code"].as_str().unwrap_or("");
            if code.len() > 100
                || !code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err("SUMMARY_REQUEST_INVALID");
            }
            let pending = rerun != 0 || args["retry"] == true;
            tx.execute("UPDATE les_summary_jobs SET status=?,claim_token='',rerun=0,last_code=?,updated_at=CURRENT_TIMESTAMP WHERE session_id=?",params![if pending{"pending"}else if code.is_empty(){"completed"}else{"failed"},code,session]).map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
            tx.commit().map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE")?;
            Ok(value)
        }
        "status" => {
            let value=conn.query_row("SELECT status,attempts,last_code FROM les_summary_jobs WHERE session_id=?",[sid(args)?],|r|Ok(json!({"status":r.get::<_,String>(0)?,"attempts":r.get::<_,i64>(1)?,"last_code":r.get::<_,String>(2)?}))).optional().map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE")?;
            Ok(value.unwrap_or(Value::Null))
        }
        _ => Err("SUMMARY_REQUEST_INVALID"),
    }
}
