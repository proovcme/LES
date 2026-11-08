//! Native empty storage, bound to a persisted index plan; never an import substitute.
use crate::{Result, Shard, database};
use serde_json::{Value, json};
use std::{fs, path::Path};
pub fn metadata(contract: &Value) -> Value {
    let dense = contract["dense_vector_name"].as_str().unwrap_or("dense");
    let sparse = contract["sparse_vector_name"]
        .as_str()
        .unwrap_or("bm25_sparse");
    let vectors = json!({dense:{"size":contract["vector_size"],"distance":"Cosine"}});
    let sparse_vectors = json!({sparse:{"modifier":"idf"}});
    json!({"schema":"les.edge-native.v1","collection":contract["collection"],"contract_fingerprint":contract["fingerprint"],
        "vectors":vectors,"sparse_vectors":sparse_vectors,"server_info":{"config":{"params":{"vectors":vectors,"sparse_vectors":sparse_vectors}}}})
}
pub fn pending(root: &Path) -> bool {
    let Ok(db) = rusqlite::Connection::open_with_flags(
        root.join("les-vector.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return false;
    };
    db.query_row(
        "SELECT metadata,phase FROM migration WHERE id=1",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )
    .ok()
    .is_some_and(|(text, phase)| {
        phase != "ready"
            && serde_json::from_str::<Value>(&text)
                .ok()
                .is_some_and(|v| v["schema"] == "les.edge-native.v1")
    })
}
pub fn create(root: &Path, contract: &Value) -> Result<Shard> {
    if !root.join("les-vector.sqlite").exists() && root.exists() {
        for entry in fs::read_dir(root).map_err(|_| "EDGE_STORAGE_UNAVAILABLE")? {
            let entry = entry.map_err(|_| "EDGE_STORAGE_UNAVAILABLE")?;
            if entry.file_name() != "content" {
                return Err("EDGE_STORAGE_NOT_EMPTY");
            }
            for child in fs::read_dir(entry.path()).map_err(|_| "EDGE_STORAGE_UNAVAILABLE")? {
                if child.map_err(|_| "EDGE_STORAGE_UNAVAILABLE")?.file_name()
                    != ".index-replacements"
                {
                    return Err("EDGE_STORAGE_NOT_EMPTY");
                }
            }
        }
    }
    let metadata = metadata(contract);
    let shard = Shard::load(root, Some(&metadata), false)?;
    let db = database(root)?;
    let phase: String = db
        .query_row("SELECT phase FROM migration WHERE id=1", [], |r| r.get(0))
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
    if phase != "ready" {
        if shard.call("count", &json!({"exact":true}))?["count"] != 0 {
            return Err("EDGE_INITIALIZATION_NOT_EMPTY");
        }
        shard.flush()?;
        // The caller holds the collection writer. Postings are initially empty;
        // normal first publication creates them before publishing a generation.
        db.execute(
            "UPDATE migration SET phase='ready',native_schema=2,error_code=NULL WHERE id=1",
            [],
        )
        .map_err(|_| "EDGE_CATALOG_UNAVAILABLE")?;
    }
    Ok(shard)
}
