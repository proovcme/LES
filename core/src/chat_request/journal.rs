//! Chat receipt and idempotency ownership; confirmed side effects are never replayed.
use super::Result;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::path::Path;
fn open(database: &Path) -> Result<Connection> {
    let db = Connection::open(database).map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    db.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS les_chat_runs(
        id TEXT PRIMARY KEY,session_key TEXT NOT NULL,request_key TEXT,request_sha TEXT NOT NULL,history_id INTEGER NOT NULL,
        status TEXT NOT NULL,response TEXT,error_code TEXT,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,UNIQUE(session_key,request_key));").map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    Ok(db)
}
fn catalog(database: &Path, action: &str, args: Value) -> Result<Value> {
    let value = crate::catalog::execute(
        &json!({"schema":"les.catalog.v1","database":database,"action":action,"arguments":args}),
    );
    if value["status"] == "completed" {
        Ok(value["value"].clone())
    } else {
        Err("CHAT_STORAGE_UNAVAILABLE")
    }
}
pub fn recover(database: &Path) -> Result<()> {
    catalog(database, "chat_history_init", json!({"safe_fallback":""}))?;
    let mut db = open(database)?;
    // Recover only this coordinator's receipts. A staged native API may share a
    // catalog with an active legacy owner; its unfinished rows are not ours.
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    let artifact = json!({"error":{"code":"CHAT_INTERRUPTED","detail":"Лес завершился до окончания ответа. Уже полученный текст сохранён; повторите запрос."},"completion_status":"interrupted"});
    tx.execute("UPDATE chat_history SET crag_status='INTERRUPTED',success=0,validation_enabled=0,artifact_json=json_patch(CASE WHEN json_valid(artifact_json) THEN artifact_json ELSE '{}' END,?) WHERE crag_status='IN_PROGRESS' AND id IN (SELECT history_id FROM les_chat_runs WHERE status='running')",[artifact.to_string()]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    tx.execute("UPDATE les_chat_runs SET status='interrupted',error_code='CHAT_INTERRUPTED',updated_at=CURRENT_TIMESTAMP WHERE status='running'",[]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    tx.commit().map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    Ok(())
}
pub fn begin(database: &Path, request: &Value, key: Option<&str>) -> Result<Value> {
    if key.is_some_and(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control)) {
        return Err("CHAT_REQUEST_KEY_INVALID");
    }
    let mut db = open(database)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    let session = request["session_id"].as_str().unwrap_or("");
    let mut normalized = request.clone();
    normalized.sort_all_objects();
    let hash = crate::sha256_hex(normalized.to_string().as_bytes());
    if let Some(key) = key {
        let saved=tx.query_row("SELECT id,request_sha,status,response,history_id FROM les_chat_runs WHERE session_key=? AND request_key=?",params![session,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,i64>(4)?))).optional().map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
        if let Some((id, previous, status, response, history)) = saved {
            if hash != previous {
                return Err("IDEMPOTENCY_KEY_REUSED");
            }
            let response = response
                .map(|s| serde_json::from_str::<Value>(&s))
                .transpose()
                .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
            return Ok(json!({"id":id,"history_id":history,"status":status,"response":response}));
        }
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    let id = crate::sha256_hex(&random);
    // History and request receipt are committed together: there is no ownerless model turn.
    let attachment_context = if request["attachment_id"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        ""
    } else {
        request["attachment_context"].as_str().unwrap_or("")
    };
    tx.execute("INSERT INTO chat_history(session_id,question,answer,crag_status,success,validation_enabled,attachment_context) VALUES(?,?,'','IN_PROGRESS',0,0,?)",
        params![request["session_id"].as_str(),request["question"].as_str().ok_or("CHAT_QUESTION_REQUIRED")?,attachment_context]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    let history = tx.last_insert_rowid();
    tx.execute("INSERT INTO les_chat_runs(id,session_key,request_key,request_sha,history_id,status) VALUES(?,?,?,?,?,'running')",params![id,session,key,hash,history]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    tx.commit().map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    Ok(json!({"id":id,"history_id":history,"status":"running","created":true}))
}
pub fn checkpoint(database: &Path, id: &str, history: i64, event: &str, text: &str) -> Result<()> {
    if text.len() > 4 * 1024 * 1024 {
        return Err("CHAT_TEXT_LIMIT");
    }
    let db = open(database)?;
    let changed=match event{
        "token"=>db.execute("UPDATE chat_history SET answer=COALESCE(answer,'')||?,tokens=COALESCE(tokens,0)+1 WHERE id=? AND crag_status='IN_PROGRESS' AND EXISTS(SELECT 1 FROM les_chat_runs WHERE id=? AND history_id=chat_history.id AND status='running')",params![text,history,id]),
        "reset"=>db.execute("UPDATE chat_history SET answer='',tokens=0 WHERE id=? AND crag_status='IN_PROGRESS' AND EXISTS(SELECT 1 FROM les_chat_runs WHERE id=? AND history_id=chat_history.id AND status='running')",params![history,id]),
        "attachment_context"=>db.execute("UPDATE chat_history SET attachment_context=? WHERE id=? AND crag_status='IN_PROGRESS' AND EXISTS(SELECT 1 FROM les_chat_runs WHERE id=? AND history_id=chat_history.id AND status='running')",params![text,history,id]),
        _=>return Err("CHAT_REQUEST_INVALID")
    }.map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    if changed != 1 {
        return Err("CHAT_REQUEST_STATE_CHANGED");
    }
    Ok(())
}
pub fn finish(
    database: &Path,
    id: &str,
    history: i64,
    row: &Value,
    response: &Value,
) -> Result<()> {
    let mut db = open(database)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    let pending = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_history WHERE id=? AND crag_status='IN_PROGRESS')",
            [history],
            |r| r.get::<_, bool>(0),
        )
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    if !pending {
        return Err("CHAT_REQUEST_STATE_CHANGED");
    }
    let changed=tx.execute("UPDATE les_chat_runs SET status='completed',response=?,updated_at=CURRENT_TIMESTAMP WHERE id=? AND history_id=? AND status='running'",params![response.to_string(),id,history]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    if changed != 1 {
        return Err("CHAT_REQUEST_STATE_CHANGED");
    }
    crate::catalog::save_chat_row(&tx, history, row).map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    tx.commit().map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    Ok(())
}
/// Save the confirmed result before another model call or a visible progress event.
/// Recovery preserves these facts but never replays their effects.
pub fn tool_result(database: &Path, id: &str, history: i64, payload: &Value) -> Result<()> {
    if payload["schema"] != "les_tool_result_v1" || payload.to_string().len() > 2 * 1024 * 1024 {
        return Err("CHAT_TOOL_RESULT_INVALID");
    }
    let mut db = open(database)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    let raw:Option<Option<String>>=tx.query_row("SELECT artifact_json FROM chat_history WHERE id=? AND crag_status='IN_PROGRESS' AND EXISTS(SELECT 1 FROM les_chat_runs WHERE id=? AND history_id=chat_history.id AND status='running')",params![history,id],|r|r.get(0)).optional().map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    let mut artifact = raw
        .ok_or("CHAT_REQUEST_STATE_CHANGED")?
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?
        .unwrap_or(json!({}));
    if !artifact.is_object() {
        return Err("CHAT_STORAGE_UNAVAILABLE");
    }
    let rows = artifact
        .as_object_mut()
        .unwrap()
        .entry("tool_results")
        .or_insert(json!([]))
        .as_array_mut()
        .ok_or("CHAT_STORAGE_UNAVAILABLE")?;
    if rows.len() >= 128 {
        return Err("CHAT_TOOL_RESULT_LIMIT");
    }
    rows.push(payload.clone());
    artifact["completion_status"] = json!("in_progress");
    let encoded = artifact.to_string();
    if encoded.len() > 16 * 1024 * 1024 {
        return Err("CHAT_TOOL_RESULT_LIMIT");
    }
    tx.execute(
        "UPDATE chat_history SET artifact_json=? WHERE id=?",
        params![encoded, history],
    )
    .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    tx.commit().map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    Ok(())
}
pub fn interrupt(database: &Path, id: &str, history: i64, error: Option<&Value>) -> Result<()> {
    let mut db = open(database)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    let changed=tx.execute("UPDATE les_chat_runs SET status=?,error_code=?,updated_at=CURRENT_TIMESTAMP WHERE id=? AND history_id=? AND status='running'",
        params![if error.is_some(){"failed"}else{"interrupted"},error.and_then(|e|e["code"].as_str()).unwrap_or("CHAT_INTERRUPTED"),id,history]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
    if changed == 1 {
        if let Some(error) = error {
            let code = error["code"]
                .as_str()
                .filter(|v| {
                    v.len() <= 100
                        && v.bytes()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                })
                .ok_or("CHAT_REQUEST_INVALID")?;
            let detail = error["detail"]
                .as_str()
                .filter(|v| v.chars().count() <= 2048)
                .ok_or("CHAT_REQUEST_INVALID")?;
            let artifact =
                json!({"error":{"code":code,"detail":detail},"completion_status":"failed"});
            tx.execute("UPDATE chat_history SET crag_status='FAILED',success=0,validation_enabled=0,artifact_json=json_patch(CASE WHEN json_valid(artifact_json) THEN artifact_json ELSE '{}' END,?) WHERE id=? AND crag_status='IN_PROGRESS'",params![artifact.to_string(),history]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
        } else {
            tx.execute("UPDATE chat_history SET crag_status='INTERRUPTED',success=0,validation_enabled=0 WHERE id=? AND crag_status='IN_PROGRESS'",[history]).map_err(|_|"CHAT_STORAGE_UNAVAILABLE")?;
        }
    }
    tx.commit().map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?;
    Ok(())
}
