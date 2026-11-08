//! Post-index outbox, always written in the caller's catalog transaction.
use rusqlite::{params, Connection};
pub(crate) fn completed(
    db: &Connection,
    id: &str,
    dataset: &str,
    now: f64,
) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS les_index_events(id TEXT PRIMARY KEY,dataset_id TEXT NOT NULL,kind TEXT NOT NULL,created_at REAL NOT NULL)")?;
    db.execute("INSERT OR IGNORE INTO les_index_events(id,dataset_id,kind,created_at) VALUES(?1,?2,'index_completed',?3)",params![id,dataset,now])?;
    Ok(())
}
