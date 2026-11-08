//! Durable batch ownership and generation-bound catalog progress.
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::File,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) const CATALOG: &str = "INDEX_BATCH_CATALOG_UNAVAILABLE";
pub(super) type Result<T> = std::result::Result<T, &'static str>;
fn cause(error: impl std::fmt::Display) -> &'static str {
    eprintln!(
        "{}",
        json!({"event":"index_batch_catalog_failed","code":CATALOG,
        "cause":error.to_string().chars().take(2000).collect::<String>()})
    );
    CATALOG
}
pub(super) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs_f64())
        .unwrap_or(0.)
}
pub(super) fn open(path: &Path) -> Result<Connection> {
    let db = crate::workspace::connection(path, true)
        .map_err(|error| cause(format!("{} sqlite={:?}", error.code, error.sqlite_extended)))?
        .ok_or(CATALOG)?;
    crate::catalog::library::register_visibility(&db).map_err(cause)?;
    Ok(db)
}
pub(super) struct Batch {
    pub id: String,
    pub db: Connection,
    dataset: String,
    _lease: File,
    finished: bool,
    recovered: bool,
}
impl Batch {
    pub fn acquire(database: &Path, dataset: &str) -> Result<Self> {
        let directory = database.parent().ok_or(CATALOG)?.join("index-batches");
        std::fs::create_dir_all(&directory).map_err(cause)?;
        let lease = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(format!("{}.lock", crate::sha256_hex(dataset.as_bytes()))))
            .map_err(cause)?;
        lease.try_lock().map_err(|_| "INDEX_BATCH_BUSY")?;
        let mut db = open(database)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS les_index_batches(
            id TEXT PRIMARY KEY,dataset_id TEXT NOT NULL,owner_pid INTEGER NOT NULL,
            status TEXT NOT NULL,started_at REAL NOT NULL,updated_at REAL NOT NULL,
            current_file TEXT NOT NULL DEFAULT '',stage TEXT NOT NULL DEFAULT '',
            stop_code TEXT NOT NULL DEFAULT '',result TEXT NOT NULL DEFAULT '{}',spool_id TEXT NOT NULL DEFAULT '');
            CREATE INDEX IF NOT EXISTS ix_les_index_batches_dataset ON les_index_batches(dataset_id,started_at);").map_err(|_|CATALOG)?;
        let columns = db
            .prepare("PRAGMA table_info(les_index_batches)")
            .map_err(cause)?
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(cause)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(cause)?;
        if !columns.iter().any(|name| name == "spool_id") {
            db.execute_batch(
                "ALTER TABLE les_index_batches ADD COLUMN spool_id TEXT NOT NULL DEFAULT ''",
            )
            .map_err(cause)?;
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(cause)?;
        let id = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(cause)?;
        if !tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM datasets WHERE id=?1)",
                [dataset],
                |r| r.get::<_, bool>(0),
            )
            .map_err(cause)?
        {
            return Err("INDEX_DATASET_NOT_FOUND");
        }
        // Holding this OS lease proves that no previous owner is still running.
        let recovered=tx.execute("UPDATE les_index_batches SET status='interrupted',stop_code='INDEX_BATCH_OWNER_LOST',updated_at=?2
            WHERE dataset_id=?1 AND status='running'",params![dataset,now()]).map_err(|_|CATALOG)?;
        tx.execute(
            "INSERT INTO les_index_batches(id,dataset_id,owner_pid,status,started_at,updated_at)
            VALUES(?1,?2,?3,'running',?4,?4)",
            params![id, dataset, std::process::id(), now()],
        )
        .map_err(cause)?;
        tx.execute(
            "UPDATE datasets SET status='PARSING' WHERE id=?1",
            [dataset],
        )
        .map_err(cause)?;
        tx.commit().map_err(cause)?;
        Ok(Self {
            id,
            db,
            dataset: dataset.into(),
            _lease: lease,
            finished: false,
            recovered: recovered > 0,
        })
    }
    pub fn spool(&self, id: &str) -> Result<()> {
        self.db
            .execute(
                "UPDATE les_index_batches SET spool_id=?2,updated_at=?3 WHERE id=?1",
                params![self.id, id, now()],
            )
            .map_err(cause)?;
        Ok(())
    }
    pub fn cleanup_spools(&self, root: &Path) -> Result<()> {
        let rows = self
            .db
            .prepare(
                "SELECT id,spool_id FROM les_index_batches WHERE dataset_id=?1 AND spool_id<>''",
            )
            .map_err(cause)?
            .query_map([&self.dataset], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(cause)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(cause)?;
        for (batch, id) in rows {
            if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("INDEX_SPOOL_CLEANUP_FAILED");
            }
            for suffix in ["nodes", "prepared.nodes", "jsonl", "cache"] {
                match std::fs::remove_file(root.join(format!("{id}.{suffix}"))) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err("INDEX_SPOOL_CLEANUP_FAILED"),
                }
            }
            self.db
                .execute(
                    "UPDATE les_index_batches SET spool_id='' WHERE id=?1 AND spool_id=?2",
                    params![batch, id],
                )
                .map_err(cause)?;
        }
        Ok(())
    }
    pub fn progress(&self, name: &str, stage: &str) -> Result<()> {
        self.db.execute("UPDATE les_index_batches SET current_file=?2,stage=?3,updated_at=?4 WHERE id=?1 AND status='running'",
            params![self.id,name,stage,now()]).map_err(|_|CATALOG)?;
        Ok(())
    }
    pub fn finish(&mut self, result: &Value, stop: &str) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(cause)?;
        let unresolved:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM documents WHERE dataset_id=?1 AND status='ERROR' AND les_visible_document(file_name))",
            [&self.dataset],|r|r.get(0)).map_err(|_|CATALOG)?;
        let status = if !stop.is_empty() || result["errors"].as_u64().unwrap_or(0) > 0 || unresolved
        {
            "ERROR"
        } else if result["remaining_pending"].as_u64().unwrap_or(0) > 0 {
            "IDLE"
        } else {
            "COMPLETED"
        };
        update_count(&tx, &self.dataset)?;
        tx.execute(
            "UPDATE datasets SET status=?2 WHERE id=?1",
            params![self.dataset, status],
        )
        .map_err(cause)?;
        tx.execute("UPDATE les_index_batches SET status=?2,stage='',stop_code=?3,result=?4,updated_at=?5 WHERE id=?1",
            params![self.id,if stop.is_empty(){"completed"}else{"interrupted"},stop,result.to_string(),now()]).map_err(|_|CATALOG)?;
        // Durable outbox shares the batch commit. A crash before API notification
        // cannot lose a requested post-index task. Historical batches create no events.
        if status == "COMPLETED"
            && stop.is_empty()
            && (result["files_parsed"].as_u64().unwrap_or(0) > 0 || self.recovered)
        {
            crate::index_events::completed(&tx, &self.id, &self.dataset, now()).map_err(cause)?;
        }
        tx.commit().map_err(cause)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for Batch {
    fn drop(&mut self) {
        if !self.finished {
            let _=self.db.execute("UPDATE les_index_batches SET status='failed',stage='',stop_code='INDEX_BATCH_ABORTED',updated_at=?2
                WHERE id=?1 AND status='running'",params![self.id,now()]);
            let _ = self.db.execute(
                "UPDATE datasets SET status='ERROR' WHERE id=?1",
                [&self.dataset],
            );
        }
    }
}
pub(super) fn update_count(db: &Connection, dataset: &str) -> Result<()> {
    db.execute("UPDATE datasets SET chunk_count=coalesce((SELECT sum(chunk_count) FROM documents WHERE dataset_id=?1 AND les_visible_document(file_name)),0) WHERE id=?1",[dataset]).map_err(|_|CATALOG)?;
    Ok(())
}
pub(super) fn registration(db: &Connection, dataset: &str, name: &str) -> Result<Option<Value>> {
    db.query_row("SELECT d.id,coalesce(d.source_path,''),d.file_mtime,d.file_size FROM documents d JOIN datasets s ON s.id=d.dataset_id
        WHERE d.dataset_id=?1 AND d.file_name=?2",params![dataset,name],|r| Ok(json!([
            r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<f64>>(2)?,r.get::<_,Option<i64>>(3)?])))
        .optional().map_err(|_|CATALOG)
}
pub(super) fn stage(
    db: &Connection,
    dataset: &str,
    name: &str,
    expected: &Value,
    stage: &str,
) -> Result<()> {
    let count=db.execute("UPDATE documents SET stage=?7 WHERE dataset_id=?1 AND file_name=?2 AND id=?3
        AND coalesce(source_path,'')=?4 AND file_mtime IS ?5 AND file_size IS ?6 AND status='PENDING'",
        params![dataset,name,expected[0].as_str(),expected[1].as_str(),expected[2].as_f64(),expected[3].as_i64(),stage]).map_err(|_|CATALOG)?;
    if count != 1 {
        return Err("INDEX_REGISTRATION_CHANGED");
    }
    Ok(())
}
pub(super) fn failure(
    db: &Connection,
    dataset: &str,
    name: &str,
    expected: &Value,
    code: &str,
    message: &str,
    retry_after: f64,
) -> Result<()> {
    db.execute("UPDATE documents SET status=?7,chunk_count=0,stage='',error_code=?8,last_error=?9,retryable=?10,retry_after=?11
        WHERE dataset_id=?1 AND file_name=?2 AND id=?3 AND coalesce(source_path,'')=?4 AND file_mtime IS ?5 AND file_size IS ?6 AND status='PENDING'",
        params![dataset,name,expected[0].as_str(),expected[1].as_str(),expected[2].as_f64(),expected[3].as_i64(),
            if code=="UNSUPPORTED_INDEXING_SOURCE"{"SKIPPED"}else{"ERROR"},code,message.chars().take(2000).collect::<String>(),retry_after>0.,retry_after]).map_err(|_|CATALOG)?;
    Ok(())
}
pub(super) fn committed(
    db: &Connection,
    dataset: &str,
    name: &str,
    id: &Value,
    hash: &str,
    count: usize,
) -> Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND dataset_id=?2 AND file_name=?3 AND status='INDEXED'
        AND file_hash=?4 AND chunk_count=?5)",params![id.as_str(),dataset,name,hash,count as i64],|r|r.get(0)).map_err(|_|CATALOG)
}
