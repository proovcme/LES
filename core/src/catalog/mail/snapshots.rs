//! Durable read-only originals and idempotent publication into the native intake queue.
use super::*;
use std::io::{Read, Write};

fn sha(bytes: &[u8]) -> String {
    crate::sha256_hex(bytes)
}
fn id_for(s: &str) -> String {
    sha(s.as_bytes())[..32].to_owned()
}
fn bound(value: &Value, key: &str, max: usize) -> Result<String> {
    let v = value[key].as_str().unwrap_or_default();
    if v.len() > max || v.chars().any(char::is_control) {
        return Err(INVALID);
    }
    Ok(v.to_owned())
}
fn root(state: &Path, account: &str) -> Result<PathBuf> {
    let mut path = state.to_path_buf();
    for part in ["mail", account, "raw"] {
        path.push(part);
        owned(&path)?;
    }
    Ok(path)
}
fn checked(path: &Path, expected: &str) -> Result<Vec<u8>> {
    owned(path)?;
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_MESSAGE as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| "MAIL_ORIGINAL_UNAVAILABLE")?;
    if bytes.len() > MAX_MESSAGE || sha(&bytes) != expected {
        return Err("MAIL_ORIGINAL_CHANGED");
    }
    Ok(bytes)
}
fn save(path: &Path, bytes: &[u8], digest: &str) -> Result<()> {
    owned(path)?;
    if path.exists() {
        return checked(path, digest).map(|_| ());
    }
    fs::create_dir_all(path.parent().ok_or(STORE)?).map_err(|_| STORE)?;
    // Registry write lease serializes publishers of the same original.
    let temp = path.with_extension(format!(
        "{}.pending",
        path.extension().and_then(|v| v.to_str()).ok_or(STORE)?
    ));
    owned(&temp)?;
    if temp.exists() {
        fs::remove_file(&temp).map_err(|_| STORE)?;
    }
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| STORE)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| STORE)?;
        drop(file);
        fs::rename(&temp, path).map_err(|_| STORE)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn parse(bytes: &[u8], suffix: &str) -> Result<Value> {
    match suffix {
        "eml" | "emlx" => crate::document::conversion::mail::read(bytes, suffix == "emlx"),
        "msg" => crate::document::conversion::msg::read(bytes),
        _ => Err("MAIL_FORMAT_UNSUPPORTED"),
    }
}
pub fn register(
    state: &Path,
    account: &str,
    suffix: &str,
    bytes: &[u8],
    fields: &Value,
) -> Result<Value> {
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE || !safe_id(account) {
        return Err(INVALID);
    }
    let parsed = parse(bytes, suffix)?;
    let meta = &parsed["metadata"];
    let digest = sha(bytes);
    let native = bound(fields, "native_id", 4096)?;
    let internet = bound(fields, "internet_message_id", 1024)?;
    let internet = if internet.is_empty() {
        meta["message_id"].as_str().unwrap_or_default()
    } else {
        &internet
    };
    let internet = internet
        .trim_matches(|c: char| c == '<' || c == '>' || c.is_whitespace())
        .to_ascii_lowercase();
    let base = if !internet.is_empty() {
        format!("mid:{internet}")
    } else if !native.is_empty() {
        format!("native:{native}")
    } else {
        format!("sha:{digest}")
    };
    let main = id_for(&format!("{account}|{base}"));
    let folder = bound(fields, "folder_id", 4096)?;
    let folder_path = bound(fields, "folder_path", 4096)?;
    let store = bound(fields, "outlook_store_id", 4096)?;
    let entry = bound(fields, "outlook_entry_id", 4096)?;
    let received = bound(fields, "received_at", 80)?;
    let date = meta["date"].as_str().unwrap_or_default();
    let received = if chrono::DateTime::parse_from_rfc3339(&received).is_ok() {
        received.as_str()
    } else {
        date
    };
    let mut conn = registry(state)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let row = raw_account(&tx, account)?;
    let kind = row["kind"].as_str().ok_or("MAIL_ACCOUNT_NOT_FOUND")?;
    if row["enabled"] != 1 {
        return Err("MAIL_ACCOUNT_DISABLED");
    }
    let current = first(
        &tx,
        "SELECT content_sha256 FROM mail_messages WHERE id=?1",
        [&main],
    )
    .map_err(db)?;
    let id = if current.is_null() || current["content_sha256"] == digest {
        main
    } else {
        id_for(&format!("{main}|{digest}"))
    };
    let filename = format!("{digest}.{suffix}");
    let path = root(state, account)?.join(&filename);
    save(&path, bytes, &digest)?;
    let now = now();
    let subject = meta["subject"].as_str().unwrap_or_default();
    let sender = meta["sender"].as_str().unwrap_or_default();
    let recipients = meta["recipients"]
        .as_str()
        .filter(|s| !s.is_empty())
        .into_iter()
        .collect::<Vec<_>>();
    let thread = meta["thread_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&internet);
    let thread = if thread.is_empty() {
        id.clone()
    } else {
        id_for(&format!("{account}|{thread}"))
    };
    let inserted=tx.execute("INSERT OR IGNORE INTO mail_messages(id,account_id,native_id,internet_message_id,content_sha256,
        subject,sender,recipients_json,received_at,sent_at,thread_key,raw_path,relative_path,source_kind,outlook_store_id,outlook_entry_id,created_at,updated_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?17)",
        params![id,account,native,internet,digest,subject,sender,json!(recipients).to_string(),received,date,thread,path.to_str().ok_or(STORE)?,
            format!("raw/{filename}"),kind,store,entry,now]).map_err(sql)?;
    if inserted == 0
        && first(
            &tx,
            "SELECT content_sha256 FROM mail_messages WHERE id=?1",
            [&id],
        )
        .map_err(db)?["content_sha256"]
            != digest
    {
        return Err("MAIL_ORIGINAL_CHANGED");
    }
    if !folder.is_empty() {
        tx.execute("INSERT INTO mail_message_locations(message_id,folder_native_id,folder_path,first_seen,last_seen)
            VALUES(?1,?2,?3,?4,?4) ON CONFLICT(message_id,folder_native_id) DO UPDATE SET
            folder_path=excluded.folder_path,last_seen=excluded.last_seen,is_current=1",params![id,folder,folder_path,now]).map_err(sql)?;
    }
    for (pos, attachment) in meta["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        if let Some(hash) = attachment["sha256"].as_str() {
            tx.execute("INSERT OR IGNORE INTO mail_attachment_provenance(account_id,attachment_sha256,message_id,attachment_id,first_seen)
                VALUES(?1,?2,?3,?4,?5)",params![account,hash,id,pos.to_string(),now]).map_err(sql)?;
        }
    }
    let parse = fields["parse"].as_bool().unwrap_or(true);
    tx.execute(
        "INSERT INTO les_mail_intents(message_id,requested) VALUES(?1,?2)
        ON CONFLICT(message_id) DO UPDATE SET requested=max(requested,excluded.requested)",
        params![id, parse],
    )
    .map_err(sql)?;
    tx.commit().map_err(sql)?;
    Ok(
        json!({"id":id,"account_id":account,"dataset_id":row["dataset_id"],"content_sha256":digest,"duplicate":inserted==0,"status":"saved","parse_requested":parse}),
    )
}

fn queue(database: &Path, state: &Path, row: &Value) -> Result<(String, String)> {
    let message = row["id"].as_str().filter(|v| safe_id(v)).ok_or(STORE)?;
    let account = row["account_id"]
        .as_str()
        .filter(|v| safe_id(v))
        .ok_or(STORE)?;
    let dataset_id = row["dataset_id"].as_str().ok_or(STORE)?;
    let name = row["dataset_name"].as_str().ok_or(STORE)?;
    let hash = row["content_sha256"].as_str().ok_or(STORE)?;
    let path = Path::new(row["raw_path"].as_str().ok_or(STORE)?);
    let suffix = path
        .extension()
        .and_then(|v| v.to_str())
        .filter(|v| matches!(*v, "eml" | "emlx" | "msg"))
        .ok_or(STORE)?;
    let filename = format!("{hash}.{suffix}");
    if path != root(state, account)?.join(&filename) {
        return Err("MAIL_ORIGINAL_UNAVAILABLE");
    }
    let size = checked(path, hash)?.len();
    dataset(database, dataset_id, name)?;
    let mut conn = connection(database, true).map_err(db)?.ok_or(STORE)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    super::super::intake::init(&tx).map_err(db)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS les_mail_index_jobs(
        job_id TEXT PRIMARY KEY, message_id TEXT NOT NULL UNIQUE, dataset_id TEXT NOT NULL, doc_id TEXT NOT NULL, sha256 TEXT NOT NULL
    )").map_err(sql)?;
    let job = id_for(&format!("les.mail.index.v1:{message}"));
    let doc = id_for(&format!("les.mail.document.v1:{message}"));
    if first(
        &tx,
        "SELECT doc_id FROM les_mail_index_jobs WHERE job_id=?1",
        [&job],
    )
    .map_err(db)?
    .is_object()
    {
        return Ok((job, doc));
    }
    let subject = row["subject"]
        .as_str()
        .unwrap_or("Без темы")
        .chars()
        .take(80)
        .map(|c| {
            if c.is_control() || "/\\:<>\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    let key = format!(
        "mail/{message}/Письмо — {}.{suffix}",
        subject.trim_end_matches([' ', '.'])
    );
    let time = super::super::intake::timestamp();
    let mtime = path
        .metadata()
        .and_then(|m| m.modified())
        .map_err(|_| STORE)?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| STORE)?
        .as_secs_f64();
    let path = path.to_str().ok_or(STORE)?;
    tx.execute(
        "INSERT INTO documents(id,dataset_id,file_name,status,file_size,file_mtime,source_path)
        VALUES(?1,?2,?3,'PENDING',?4,?5,?6)",
        params![doc, dataset_id, key, size as i64, mtime, path],
    )
    .map_err(sql)?;
    tx.execute("INSERT INTO native_intakes(id,dataset_id,dataset_name,root,parse_limit,status,phase,total,processed,index_pending,message,started_at,updated_at)
        VALUES(?1,?2,?3,?4,1,'queued','index',1,1,1,'Письмо в очереди на индексацию',?5,?5)",
        params![job,dataset_id,name,INDEX_ROOT,time]).map_err(sql)?;
    tx.execute(
        "INSERT INTO native_intake_files(job_id,ordinal,file_name,source_path) VALUES(?1,0,?2,?3)",
        params![job, key, path],
    )
    .map_err(sql)?;
    tx.execute(
        "INSERT INTO les_mail_index_jobs VALUES(?1,?2,?3,?4,?5)",
        params![job, message, dataset_id, doc, hash],
    )
    .map_err(sql)?;
    tx.commit().map_err(sql)?;
    Ok((job, doc))
}
pub fn publish_pending(state: &Path, database: &Path) -> Result<()> {
    if !state.join("data/mail_registry.db").is_file() {
        return Ok(());
    }
    let mut conn = registry(state)?;
    let pending = rows(
        &conn,
        "SELECT m.*,a.dataset_id,a.dataset_name FROM les_mail_intents i
        JOIN mail_messages m ON m.id=i.message_id JOIN mail_accounts a ON a.id=m.account_id
        WHERE i.requested=1 AND i.job_id='' AND a.enabled=1
        ORDER BY i.detail<>'',m.created_at,m.id LIMIT 32",
        [],
    )
    .map_err(db)?;
    for row in pending {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let id = row["id"].as_str().ok_or(STORE)?;
        match queue(database, state, &row) {
            Ok((job, doc)) => {
                tx.execute(
                    "UPDATE mail_messages SET rag_doc_id=?2,index_status='registered' WHERE id=?1",
                    params![id, doc],
                )
                .map_err(sql)?;
                tx.execute(
                    "UPDATE les_mail_intents SET job_id=?2,detail='' WHERE message_id=?1",
                    params![id, job],
                )
                .map_err(sql)?;
            }
            Err(code) => {
                tx.execute(
                    "UPDATE les_mail_intents SET detail=?2 WHERE message_id=?1",
                    params![id, code],
                )
                .map_err(sql)?;
            }
        }
        tx.commit().map_err(sql)?;
    }
    Ok(())
}

pub fn index_authorized(database: &Path, dataset: &str, job: &str, hash: &str) -> Result<bool> {
    let conn = connection(database, false).map_err(db)?.ok_or(STORE)?;
    if !private_dataset(&conn, dataset)? || !table(&conn, "les_mail_index_jobs").map_err(db)? {
        return Ok(false);
    }
    conn.query_row("SELECT EXISTS(SELECT 1 FROM les_mail_index_jobs i JOIN native_intakes n ON n.id=i.job_id
        WHERE i.job_id=?1 AND i.dataset_id=?2 AND i.sha256=?3 AND n.status='running' AND n.finished_at IS NULL)",
        params![job,dataset,hash],|r|r.get(0)).map_err(sql)
}
pub fn cursor(state: &Path, account: &str, folder: &str) -> Result<Value> {
    first(&registry(state)?,"SELECT uid_validity,last_uid,backfill_complete,enabled FROM mail_folders WHERE account_id=?1 AND native_id=?2",
        params![account,folder]).map_err(db)
}
pub fn folder_checkpoint(
    state: &Path,
    account: &str,
    folder: &str,
    name: &str,
    validity: u32,
    uid: u32,
    complete: bool,
) -> Result<()> {
    registry(state)?.execute("INSERT INTO mail_folders(account_id,native_id,path,uid_validity,last_uid,backfill_complete,updated_at)
        VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(account_id,native_id) DO UPDATE SET
        path=excluded.path,uid_validity=excluded.uid_validity,last_uid=excluded.last_uid,backfill_complete=excluded.backfill_complete,updated_at=excluded.updated_at",
        params![account,folder,name,validity.to_string(),uid,complete,now()]).map_err(sql)?;
    Ok(())
}
pub fn message(state: &Path, database: &Path, id: &str) -> Result<Value> {
    if !safe_id(id) {
        return Err(INVALID);
    }
    let conn = registry(state)?;
    let mut row=first(&conn,"SELECT m.*,i.job_id,i.detail AS index_error FROM mail_messages m LEFT JOIN les_mail_intents i ON i.message_id=m.id WHERE m.id=?1",[id]).map_err(db)?;
    if row.is_null() {
        return Err("MAIL_MESSAGE_NOT_FOUND");
    }
    row["locations"]=json!(rows(&conn,"SELECT folder_native_id,folder_path,last_seen,is_current FROM mail_message_locations WHERE message_id=?1 ORDER BY last_seen DESC",[id]).map_err(db)?);
    if let (Some(doc), Some(main)) = (
        row["rag_doc_id"].as_str().filter(|v| !v.is_empty()),
        connection(database, false).map_err(db)?,
    ) {
        let document =
            first(&main, "SELECT status FROM documents WHERE id=?1", [doc]).map_err(db)?;
        if document["status"].is_string() {
            row["index_status"] = document["status"].clone();
        }
    }
    Ok(row)
}
pub fn read(state: &Path, database: &Path, id: &str) -> Result<Value> {
    let mut row = message(state, database, id)?;
    let account = row["account_id"]
        .as_str()
        .filter(|v| safe_id(v))
        .ok_or(STORE)?;
    let hash = row["content_sha256"].as_str().ok_or(STORE)?;
    let path = Path::new(row["raw_path"].as_str().ok_or(STORE)?);
    let suffix = path.extension().and_then(|v| v.to_str()).ok_or(STORE)?;
    if path != root(state, account)?.join(format!("{hash}.{suffix}")) {
        return Err("MAIL_ORIGINAL_UNAVAILABLE");
    }
    row["profile"] = parse(&checked(path, hash)?, suffix)?;
    row.as_object_mut().ok_or(STORE)?.remove("raw_path");
    Ok(row)
}
pub fn list(
    state: &Path,
    account: Option<&str>,
    thread: Option<&str>,
    limit: u32,
) -> Result<Value> {
    let conn = registry(state)?;
    let found=rows(&conn,"SELECT id,account_id,internet_message_id,subject,sender,received_at,thread_key,source_kind,index_status,rag_doc_id
        FROM mail_messages WHERE (?1 IS NULL OR account_id=?1) AND (?2 IS NULL OR thread_key=?2)
        ORDER BY received_at DESC,id LIMIT ?3",params![account,thread,limit.clamp(1,200)]).map_err(db)?;
    Ok(json!({"messages":found,"count":found.len()}))
}
