//! Native mailbox registry. The existing SQLite identities remain readable.
mod credentials;
mod olm;
mod snapshots;
mod threads;
use super::{connection, first, rows, table, Failure};
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub use olm::visit_olm;
pub use snapshots::{
    cursor, folder_checkpoint, index_authorized, list, message, publish_pending, read, register,
};
pub use threads::{rebuild_threads, threads};
type Result<T> = std::result::Result<T, &'static str>;
const STORE: &str = "MAIL_REGISTRY_UNAVAILABLE";
const INVALID: &str = "MAIL_REQUEST_INVALID";
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
pub const INDEX_ROOT: &str = "les:mail-index:v1";

pub fn encode_mime_base64(bytes: &[u8]) -> String {
    crate::chat_attachments::encode_base64(bytes)
}

pub fn decode_mapi_text(bytes: &[u8], codepage: u32) -> String {
    use mail_parser::decoders::charsets::map::charset_decoder;
    let bytes = &bytes[..bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1)];
    let name = match codepage {
        65001 => "utf-8".to_owned(),
        932 => "shift_jis".to_owned(),
        936 => "gbk".to_owned(),
        949 => "euc-kr".to_owned(),
        950 => "big5".to_owned(),
        20866 => "koi8-r".to_owned(),
        28591..=28606 => format!("iso-8859-{}", codepage - 28590),
        _ => format!("cp{codepage}"),
    };
    if let Some(decode) = charset_decoder(name.as_bytes()) {
        return decode(bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn filetime_rfc2822(ticks: i64) -> Option<String> {
    let seconds = ticks.checked_div(10_000_000)?.checked_sub(11_644_473_600)?;
    let date = chrono::DateTime::from_timestamp(seconds, 0)?;
    use chrono::Datelike;
    (1601..=9999)
        .contains(&date.year())
        .then(|| date.to_rfc2822())
}

fn db(_: Failure) -> &'static str {
    STORE
}
fn sql(_: rusqlite::Error) -> &'static str {
    STORE
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
fn owned(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(INVALID);
    }
    match crate::chat_attachments::regular(path) {
        Ok(_) => Ok(()),
        Err(_) => Err(STORE),
    }
}
fn registry(state: &Path) -> Result<Connection> {
    owned(state)?;
    let data = state.join("data");
    owned(&data)?;
    fs::create_dir_all(&data).map_err(|_| STORE)?;
    let path = data.join("mail_registry.db");
    owned(&path)?;
    let mut conn = connection(&path, true).map_err(db)?.ok_or(STORE)?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(sql)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    tx.execute_batch(include_str!("schema.sql")).map_err(sql)?;
    for column in ["dataset_id", "dataset_name"] {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('mail_accounts') WHERE name=?1)",
                [column],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !exists {
            tx.execute_batch(&format!(
                "ALTER TABLE mail_accounts ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
            ))
            .map_err(sql)?;
        }
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS les_mail_intents(message_id TEXT PRIMARY KEY REFERENCES mail_messages(id),
        requested INTEGER NOT NULL, job_id TEXT NOT NULL DEFAULT '', detail TEXT NOT NULL DEFAULT '')").map_err(sql)?;
    tx.commit().map_err(sql)?;
    Ok(conn)
}
fn safe_id(id: &str) -> bool {
    !matches!(id, "" | "." | "..")
        && id.len() <= 160
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn token(v: &Value, max: usize) -> Result<&str> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .ok_or(INVALID)
}
fn projection(mut row: Value) -> Result<Value> {
    if row.is_null() {
        return Err("MAIL_ACCOUNT_NOT_FOUND");
    }
    let mut config = row
        .as_object_mut()
        .and_then(|m| m.remove("config_json"))
        .and_then(|v| serde_json::from_str::<Value>(v.as_str()?).ok())
        .filter(Value::is_object)
        .ok_or(STORE)?;
    config.as_object_mut().unwrap().remove("password");
    row["config"] = config;
    row["enabled"] = json!(row["enabled"] == 1);
    row["secret_set"] = json!(
        row["kind"] == "imap"
            && row["id"]
                .as_str()
                .is_some_and(|id| safe_id(id) && credentials::get(id).is_ok_and(|v| v.is_some()))
    );
    Ok(row)
}
fn raw_account(conn: &Connection, id: &str) -> Result<Value> {
    if !safe_id(id) {
        return Err(INVALID);
    }
    first(conn, "SELECT * FROM mail_accounts WHERE id=?1", [id]).map_err(db)
}
pub fn account(state: &Path, id: &str) -> Result<Value> {
    projection(raw_account(&registry(state)?, id)?)
}
pub fn enabled_imap_accounts(state: &Path) -> Result<Vec<String>> {
    let conn = registry(state)?;
    let mut query = conn
        .prepare("SELECT id FROM mail_accounts WHERE kind='imap' AND enabled=1 ORDER BY id")
        .map_err(sql)?;
    let ids = query.query_map([], |row| row.get(0)).map_err(sql)?;
    ids.collect::<std::result::Result<Vec<_>, _>>().map_err(sql)
}
pub fn secret(id: &str) -> Result<String> {
    if !safe_id(id) {
        return Err(INVALID);
    }
    credentials::get(id)?.ok_or("MAIL_SECRET_MISSING")
}
pub fn accounts(state: &Path) -> Result<Value> {
    let conn = registry(state)?;
    let accounts = rows(
        &conn,
        "SELECT * FROM mail_accounts ORDER BY label COLLATE NOCASE,id",
        [],
    )
    .map_err(db)?
    .into_iter()
    .map(projection)
    .collect::<Result<Vec<_>>>()?;
    let mut folders = rows(
        &conn,
        "SELECT * FROM mail_folders ORDER BY account_id,path",
        [],
    )
    .map_err(db)?;
    for row in &mut folders {
        row["enabled"] = json!(row["enabled"] == 1);
        row["backfill_complete"] = json!(row["backfill_complete"] == 1);
    }
    Ok(json!({"component":"Е.Ж.И.К.","accounts":accounts,"folders":folders}))
}
fn mailbox_config(kind: &str, input: &Value, old: &Value) -> Result<Value> {
    if kind == "archive" {
        return Ok(json!({}));
    }
    if kind == "outlook_classic" {
        return Ok(json!({"exclude_special":["junk","trash","drafts"]}));
    }
    if kind != "imap" {
        return Err(INVALID);
    }
    let field = |k: &str| {
        input
            .get(k)
            .filter(|v| !v.is_null())
            .or_else(|| old.get(k))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let provider = field("provider")
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let host_field = field("host");
    let host = if provider == "yandex" {
        "imap.yandex.ru"
    } else {
        token(&host_field, 253)?
    };
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b))
        || host.starts_with('-')
    {
        return Err(INVALID);
    }
    let login = field("login");
    let login = token(&login, 1024)?;
    let port = if provider == "yandex" {
        993
    } else {
        field("port").as_u64().unwrap_or(993)
    };
    let ssl = provider == "yandex" || field("ssl").as_bool().unwrap_or(true);
    if !(1..=65535).contains(&port) {
        return Err(INVALID);
    }
    let folders = field("folders");
    let folders = if folders.is_null() {
        vec!["*"]
    } else {
        let arr = folders
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or(INVALID)?;
        arr.iter()
            .map(|v| token(v, 1024))
            .collect::<Result<Vec<_>>>()?
    };
    Ok(
        json!({"provider":provider,"host":host,"port":port,"login":login,"ssl":ssl,"folders":folders}),
    )
}

pub fn imap_config(input: &Value) -> Result<Value> {
    mailbox_config("imap", input, &Value::Null)
}
fn dataset(database: &Path, id: &str, name: &str) -> Result<()> {
    if !safe_id(id) {
        return Err(INVALID);
    }
    let mut conn = connection(database, true).map_err(db)?.ok_or(STORE)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    super::library::initialize_empty(&tx).map_err(db)?;
    let row = first(
        &tx,
        "SELECT name,sensitivity,dataset_scope FROM datasets WHERE id=?1",
        [id],
    )
    .map_err(db)?;
    if row.is_null() {
        tx.execute("INSERT INTO datasets(id,name,status,sensitivity,dataset_scope) VALUES(?1,?2,'IDLE','P0','user')",params![id,name]).map_err(sql)?;
    } else if row["sensitivity"] != "P0" || row["dataset_scope"] == "system" {
        return Err("MAIL_DATASET_NOT_PRIVATE");
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS les_mail_private_datasets(dataset_id TEXT PRIMARY KEY)",
    )
    .map_err(sql)?;
    tx.execute(
        "INSERT OR IGNORE INTO les_mail_private_datasets(dataset_id) VALUES(?1)",
        [id],
    )
    .map_err(sql)?;
    tx.commit().map_err(sql)
}
fn dataset_name(label: &str, id: &str) -> String {
    let safe = label
        .chars()
        .map(|v| {
            if v.is_alphanumeric() || "_.@+-".contains(v) {
                v
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("MAIL_{}_{}_Index", safe.trim_matches(['.', '_']), &id[..8])
}
pub fn create(state: &Path, database: &Path, input: &Value, collector: bool) -> Result<Value> {
    let label = token(&input["label"], 640)?;
    if label.chars().count() > 160 {
        return Err(INVALID);
    }
    let kind = token(&input["kind"], 32)?;
    let config = mailbox_config(kind, input, &Value::Null)?;
    let native = input["native_account_id"].as_str().unwrap_or("");
    if native.len() > 4096
        || native.chars().any(char::is_control)
        || (collector && (!matches!(kind, "outlook_classic" | "archive") || native.is_empty()))
    {
        return Err(INVALID);
    }
    let mut conn = registry(state)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    if collector {
        let row = first(
            &tx,
            "SELECT * FROM mail_accounts WHERE kind=?1 AND native_account_id=?2",
            [kind, native],
        )
        .map_err(db)?;
        if row.is_object() {
            dataset(
                database,
                row["dataset_id"].as_str().ok_or(STORE)?,
                row["dataset_name"].as_str().ok_or(STORE)?,
            )?;
            return projection(row);
        }
    }
    let id:String=tx.query_row("SELECT lower(hex(randomblob(4))||'-'||hex(randomblob(2))||'-4'||substr(hex(randomblob(2)),2)||'-8'||substr(hex(randomblob(2)),2)||'-'||hex(randomblob(6)))",[],|r|r.get(0)).map_err(sql)?;
    let name = dataset_name(label, &id);
    let password = input["password"].as_str().unwrap_or("");
    if password.len() > 2560 || password.chars().any(char::is_control) {
        return Err(INVALID);
    }
    tx.execute("INSERT INTO mail_accounts(id,kind,label,dataset_id,dataset_name,native_account_id,config_json,created_at,updated_at)
        VALUES(?1,?2,?3,?1,?4,?5,?6,?7,?7)",params![id,kind,label,name,native,config.to_string(),now()]).map_err(sql)?;
    dataset(database, &id, &name)?;
    if kind == "imap" && !password.is_empty() {
        credentials::set(&id, password)?;
    }
    tx.commit().map_err(sql)?;
    account(state, &id)
}
pub fn update(state: &Path, id: &str, input: &Value) -> Result<Value> {
    let mut conn = registry(state)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql)?;
    let row = projection(raw_account(&tx, id)?)?;
    if row["sync_state"] == "running" {
        return Err("MAIL_SYNC_BUSY");
    }
    let kind = row["kind"].as_str().ok_or(STORE)?;
    let config = mailbox_config(kind, input, &row["config"])?;
    let label = if input["label"].is_null() {
        row["label"].as_str().ok_or(STORE)?
    } else {
        token(&input["label"], 640)?
    };
    let enabled = input["enabled"].as_bool().unwrap_or(row["enabled"] == true);
    let password = input["password"].as_str();
    if password.is_some_and(|v| v.len() > 2560 || v.chars().any(char::is_control)) {
        return Err(INVALID);
    }
    if kind == "imap"
        && row["secret_set"] == true
        && password.is_none()
        && ["host", "port", "login", "ssl"]
            .iter()
            .any(|key| config[*key] != row["config"][*key])
    {
        return Err("MAIL_SECRET_RECONFIRM_REQUIRED");
    }
    tx.execute(
        "UPDATE mail_accounts SET label=?2,enabled=?3,config_json=?4,updated_at=?5 WHERE id=?1",
        params![id, label, enabled, config.to_string(), now()],
    )
    .map_err(sql)?;
    if kind == "imap" {
        if let Some(password) = password {
            credentials::set(id, password)?;
        }
    }
    tx.commit().map_err(sql)?;
    account(state, id)
}
pub fn sync_status(state: &Path, id: &str, status: &str, detail: &str) -> Result<()> {
    registry(state)?
        .execute(
            "UPDATE mail_accounts SET sync_state=?2,last_error=?3,
        last_sync=CASE WHEN ?2='idle' THEN ?4 ELSE last_sync END,updated_at=?4 WHERE id=?1",
            params![id, status, detail, now()],
        )
        .map_err(sql)?;
    Ok(())
}
pub fn recover_sync(state: &Path) -> Result<()> {
    registry(state)?
        .execute(
            "UPDATE mail_accounts SET sync_state='interrupted',
        last_error='MAIL_SYNC_INTERRUPTED' WHERE sync_state='running'",
            [],
        )
        .map_err(sql)?;
    Ok(())
}
pub fn private_dataset(conn: &Connection, id: &str) -> Result<bool> {
    if !table(conn, "les_mail_private_datasets").map_err(db)? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM les_mail_private_datasets WHERE dataset_id=?1)",
        [id],
        |r| r.get(0),
    )
    .map_err(sql)
}
pub fn is_private_dataset(database: &Path, id: &str) -> Result<bool> {
    match connection(database, false).map_err(db)? {
        Some(conn) => private_dataset(&conn, id),
        None => Ok(false),
    }
}
