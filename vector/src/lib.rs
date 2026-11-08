//! Product-owned embedded vector storage. No Python and no background database service.
pub mod initialize;
pub mod migration;
pub mod operations;
pub mod postings;
mod retrieval;
use qdrant_edge::*;
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, &'static str>;
pub struct Shard {
    pub(crate) edge: EdgeShard,
    pub(crate) root: PathBuf,
    pub(crate) metadata: Value,
    _owner: File,
}
pub fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 128 * 1024];
    loop {
        let n = file
            .read(&mut bytes)
            .map_err(|_| "EDGE_SOURCE_UNAVAILABLE")?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(crate) fn database(root: &Path) -> Result<Connection> {
    let db =
        Connection::open(root.join("les-vector.sqlite")).map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
    db.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS migration(id INTEGER PRIMARY KEY CHECK(id=1), metadata TEXT NOT NULL,
          phase TEXT NOT NULL, next_byte INTEGER NOT NULL DEFAULT 0, points INTEGER NOT NULL DEFAULT 0,
          error_code TEXT);").map_err(|_|"EDGE_CATALOG_UNAVAILABLE")?;
    let exists = db
        .prepare("PRAGMA table_info(migration)")
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?
        .any(|v| v.is_ok_and(|name| name == "native_schema"));
    if !exists {
        db.execute_batch(
            "ALTER TABLE migration ADD COLUMN native_schema INTEGER NOT NULL DEFAULT 1",
        )
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
    }
    Ok(db)
}
impl Shard {
    pub fn open(root: &Path) -> Result<Self> {
        Self::load(root, None, true)
    }
    pub(crate) fn load(root: &Path, metadata: Option<&Value>, ready: bool) -> Result<Self> {
        if !root.is_absolute() {
            return Err("EDGE_PATH_INVALID");
        }
        if ready && !root.join("les-vector.sqlite").is_file() {
            return Err("EDGE_MIGRATION_NOT_READY");
        }
        fs::create_dir_all(root).map_err(|_| "EDGE_STORAGE_UNAVAILABLE")?;
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("owner.lock"))
            .map_err(|_| "EDGE_STORAGE_UNAVAILABLE")?;
        owner.try_lock().map_err(|_| "EDGE_STORAGE_BUSY")?;
        let root = fs::canonicalize(root).map_err(|_| "EDGE_STORAGE_UNAVAILABLE")?;
        let db = database(&root)?;
        let existing = db
            .query_row("SELECT metadata,phase FROM migration WHERE id=1", [], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()
            .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
        let metadata = match (existing, metadata) {
            (Some((saved, phase)), requested) => {
                let saved: Value =
                    serde_json::from_str(&saved).map_err(|_| "EDGE_CATALOG_INVALID")?;
                if requested.is_some_and(|v| *v != saved) {
                    return Err("EDGE_MIGRATION_SOURCE_CHANGED");
                }
                if ready && phase != "ready" {
                    return Err("EDGE_MIGRATION_NOT_READY");
                }
                if ready
                    && db
                        .query_row("SELECT native_schema FROM migration WHERE id=1", [], |r| {
                            r.get::<_, i64>(0)
                        })
                        .unwrap_or(0)
                        < 2
                {
                    return Err("EDGE_POSTINGS_NOT_READY");
                }
                saved
            }
            (None, Some(requested)) => {
                db.execute(
                    "INSERT INTO migration(id,metadata,phase) VALUES(1,?1,'importing')",
                    [requested.to_string()],
                )
                .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
                requested.clone()
            }
            _ => return Err("EDGE_MIGRATION_NOT_READY"),
        };
        let mut vectors = metadata["vectors"].clone();
        if vectors.get("size").is_some() {
            vectors = json!({"":vectors});
        }
        if !vectors.is_object() {
            return Err("EDGE_VECTOR_CONFIG_INVALID");
        }
        for params in vectors.as_object_mut().unwrap().values_mut() {
            params["memory"] = json!("cold");
        }
        let sparse = metadata
            .get("sparse_vectors")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut config = EdgeConfigBuilder::new()
            .payload_memory(Memory::Cold)
            .max_search_threads(2)
            .wal_enabled(true)
            .wal_options(WalOptions {
                segment_capacity: 4 * 1024 * 1024,
                ..Default::default()
            })
            .hnsw_config(HnswIndexConfig {
                full_scan_threshold: 64,
                max_indexing_threads: 4,
                ..Default::default()
            })
            .optimizers(EdgeOptimizersConfig {
                default_segment_number: Some(2),
                indexing_threshold: Some(1024),
                ..Default::default()
            })
            .build();
        config.vectors =
            serde_json::from_value(vectors).map_err(|_| "EDGE_VECTOR_CONFIG_INVALID")?;
        config.sparse_vectors =
            serde_json::from_value(sparse).map_err(|_| "EDGE_VECTOR_CONFIG_INVALID")?;
        let path = root.join("shard");
        let edge = if path.exists() {
            EdgeShard::load(&path, None)
        } else {
            fs::create_dir(&path).map_err(|_| "EDGE_STORAGE_UNAVAILABLE")?;
            EdgeShard::new(&path, config)
        }
        .map_err(|_| "EDGE_STORAGE_OPEN_FAILED")?;
        Ok(Self {
            edge,
            root,
            metadata,
            _owner: owner,
        })
    }
    pub fn collection(&self) -> &str {
        self.metadata["collection"].as_str().unwrap_or("")
    }
    pub fn info(&self) -> Result<Value> {
        let info = self.edge.info().map_err(|_| "EDGE_READ_FAILED")?;
        Ok(
            json!({"engine":"qdrant-edge","version":"0.9.0","collection":self.collection(),"points_count":info.points_count,"payload_schema":info.payload_schema,"indexed_vectors_count":info.indexed_vectors_count,"segments_count":info.segments_count}),
        )
    }
    pub fn flush(&self) -> Result<()> {
        self.edge.flush().map_err(|_| "EDGE_FLUSH_FAILED")
    }
    pub fn optimize(&self) -> Result<()> {
        self.edge.optimize().map_err(|_| "EDGE_OPTIMIZE_FAILED")?;
        self.flush()
    }
}
use rusqlite::OptionalExtension;
