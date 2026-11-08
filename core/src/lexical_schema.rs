//! Additive FTS projection migration shared by bootstrap, publication and retained readers.
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{json, Value};
use std::path::Path;
const ERROR: &str = "LEXICAL_SCHEMA_UNAVAILABLE";
pub fn initialize(database: &Path) -> Result<(), &'static str> {
    if !database.is_absolute() {
        return Err("LEXICAL_SCHEMA_PATH_INVALID");
    }
    let mut conn = Connection::open(database).map_err(|_| ERROR)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| ERROR)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ERROR)?;
    let existed: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='lexical_chunks_fts')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| ERROR)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS lexical_chunks(
        id INTEGER PRIMARY KEY AUTOINCREMENT,collection TEXT NOT NULL,point_id TEXT NOT NULL,
        dataset_id TEXT DEFAULT '',doc_id TEXT DEFAULT '',doc_name TEXT DEFAULT '',text TEXT NOT NULL,
        content_hash TEXT NOT NULL,chunk_ord INTEGER,section_heading TEXT DEFAULT '',
        parent_id TEXT DEFAULT '',parent_ord INTEGER,child_ord INTEGER,parent_heading TEXT DEFAULT '',
        context_before TEXT DEFAULT '',context_after TEXT DEFAULT '',context_kind TEXT DEFAULT '',
        updated_at REAL NOT NULL,UNIQUE(collection,point_id));
        CREATE TABLE IF NOT EXISTS lexical_index_meta(collection TEXT PRIMARY KEY,point_count INTEGER DEFAULT 0,
            indexed_count INTEGER DEFAULT 0,cursor_json TEXT DEFAULT '',updated_at REAL NOT NULL);").map_err(|_|ERROR)?;
    let columns = tx
        .prepare("PRAGMA table_info(lexical_chunks)")
        .map_err(|_| ERROR)?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|_| ERROR)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ERROR)?;
    for (name, ddl) in [
        ("parent_id", "parent_id TEXT DEFAULT ''"),
        ("parent_ord", "parent_ord INTEGER"),
        ("child_ord", "child_ord INTEGER"),
        ("parent_heading", "parent_heading TEXT DEFAULT ''"),
        ("context_before", "context_before TEXT DEFAULT ''"),
        ("context_after", "context_after TEXT DEFAULT ''"),
        ("context_kind", "context_kind TEXT DEFAULT ''"),
    ] {
        if !columns.iter().any(|v| v == name) {
            tx.execute_batch(&format!("ALTER TABLE lexical_chunks ADD COLUMN {ddl}"))
                .map_err(|_| ERROR)?;
        }
    }
    tx.execute_batch("CREATE VIRTUAL TABLE IF NOT EXISTS lexical_chunks_fts USING fts5(text,doc_name,content='lexical_chunks',content_rowid='id',tokenize='unicode61');
        CREATE TRIGGER IF NOT EXISTS lexical_chunks_ai AFTER INSERT ON lexical_chunks BEGIN INSERT INTO lexical_chunks_fts(rowid,text,doc_name) VALUES(new.id,new.text,new.doc_name); END;
        CREATE TRIGGER IF NOT EXISTS lexical_chunks_ad AFTER DELETE ON lexical_chunks BEGIN INSERT INTO lexical_chunks_fts(lexical_chunks_fts,rowid,text,doc_name) VALUES('delete',old.id,old.text,old.doc_name); END;
        CREATE TRIGGER IF NOT EXISTS lexical_chunks_au AFTER UPDATE ON lexical_chunks BEGIN
            INSERT INTO lexical_chunks_fts(lexical_chunks_fts,rowid,text,doc_name) VALUES('delete',old.id,old.text,old.doc_name);
            INSERT INTO lexical_chunks_fts(rowid,text,doc_name) VALUES(new.id,new.text,new.doc_name); END;
        CREATE INDEX IF NOT EXISTS idx_lexical_neighbors ON lexical_chunks(collection,dataset_id,doc_name,chunk_ord);
        CREATE INDEX IF NOT EXISTS idx_lexical_parent ON lexical_chunks(collection,parent_id);").map_err(|_|ERROR)?;
    if !existed {
        tx.execute_batch("INSERT INTO lexical_chunks_fts(lexical_chunks_fts) VALUES('rebuild')")
            .map_err(|_| ERROR)?;
    }
    tx.commit().map_err(|_| ERROR)
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    initialize(Path::new(
        request["database"]
            .as_str()
            .ok_or("LEXICAL_SCHEMA_PATH_INVALID")?,
    ))?;
    Ok(json!({"status":"ready"}))
}
