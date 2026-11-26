//! Exact count records of one registered RVT revision.
use super::{digest, identity};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::path::Path;

const SCHEMA: &str = "les.rvt-count-records.v1";
const STORAGE: &str = "RVT_SNAPSHOT_STORAGE_FAILED";
const MAX_BYTES: usize = 64 * 1024 * 1024;

fn database(path: &Path, write: bool) -> Result<Option<Connection>, &'static str> {
    crate::workspace::connection(path, write).map_err(|_| STORAGE)
}

pub(crate) fn save(
    request: &Value,
    source_sha256: &str,
    decoded: &Value,
) -> Result<(), &'static str> {
    let Some(target) = request.get("rvt_count_snapshot") else {
        return Ok(());
    };
    let path = target["database"]
        .as_str()
        .map(Path::new)
        .filter(|p| p.is_absolute())
        .ok_or(STORAGE)?;
    let document = identity(&target["registration"][0])?;
    let dataset = identity(&request["dataset_id"])?;
    let name = identity(&target["file_name"])?;
    let objects = decoded["objects"]
        .as_array()
        .ok_or("ENGINEERING_READER_FAILED")?
        .iter()
        .filter(|o| matches!(o["section"].as_str(), Some("elements" | "types")))
        .map(|o| {
            json!({
                "object_id":o["object_id"], "native_id":o["native_id"], "section":o["section"],
                "name":o["name"], "identifiers":o["identifiers"],
                "category_id":o["properties"]["rvt_category_id"], "model_context":o["model_context"]
            })
        })
        .collect::<Vec<_>>();
    let payload = serde_json::to_vec(&json!({"schema":SCHEMA,"source_sha256":source_sha256,
        "reader":decoded["reader"],"coverage":decoded["coverage"],"objects":objects}))
    .map_err(|_| STORAGE)?;
    if payload.len() > MAX_BYTES {
        return Err("RVT_SNAPSHOT_LIMIT");
    }
    let mut db = database(path, true)?.ok_or(STORAGE)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| STORAGE)?;
    let current = tx
        .query_row(
            "SELECT coalesce(file_hash,'') FROM documents
         WHERE id=?1 AND dataset_id=?2 AND file_name=?3 AND status='PENDING'
           AND coalesce(source_path,'')=?4 AND file_mtime IS ?5 AND file_size IS ?6",
            params![
                document,
                dataset,
                name,
                target["registration"][1].as_str(),
                target["registration"][2].as_f64(),
                target["registration"][3].as_i64()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| STORAGE)?
        .ok_or("INDEX_REGISTRATION_CHANGED")?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS les_rvt_count_snapshots(
            doc_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            source_sha256 TEXT NOT NULL,
            payload BLOB NOT NULL,
            PRIMARY KEY(doc_id,source_sha256)
         );
         CREATE TRIGGER IF NOT EXISTS les_rvt_count_snapshot_delete AFTER DELETE ON documents
         BEGIN DELETE FROM les_rvt_count_snapshots WHERE doc_id=OLD.id; END;",
    )
    .map_err(|_| STORAGE)?;
    tx.execute(
        "INSERT INTO les_rvt_count_snapshots(doc_id,source_sha256,payload) VALUES(?1,?2,?3)
         ON CONFLICT(doc_id,source_sha256) DO UPDATE SET payload=excluded.payload",
        params![document, source_sha256, payload],
    )
    .map_err(|_| STORAGE)?;
    tx.execute(
        "DELETE FROM les_rvt_count_snapshots WHERE doc_id=?1 AND source_sha256 NOT IN (?2,?3)",
        params![document, source_sha256, current],
    )
    .map_err(|_| STORAGE)?;
    tx.commit().map_err(|_| STORAGE)
}

pub fn load(db_path: &Path, doc: &Value) -> Result<Option<Value>, &'static str> {
    let Ok(source_sha256) = digest(&doc["file_hash"]) else {
        return Ok(None);
    };
    if doc["status"] != "INDEXED" {
        return Ok(None);
    }
    let Some(db) = database(db_path, false)? else {
        return Ok(None);
    };
    if !db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='les_rvt_count_snapshots' AND type='table')",
        [],
        |row| row.get::<_, bool>(0),
    ).map_err(|_| STORAGE)? {
        return Ok(None);
    }
    let payload = db
        .query_row(
            "SELECT s.payload FROM les_rvt_count_snapshots s JOIN documents d ON d.id=s.doc_id
         WHERE d.id=?1 AND d.dataset_id=?2 AND d.file_name=?3 AND d.file_hash=?4
           AND d.status='INDEXED' AND s.source_sha256=d.file_hash AND length(s.payload)<=?5",
            params![
                identity(&doc["id"])?,
                identity(&doc["dataset_id"])?,
                identity(&doc["file_name"])?,
                source_sha256,
                MAX_BYTES as i64
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|_| STORAGE)?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_slice(&payload).map_err(|_| STORAGE)?;
    if payload["schema"] != SCHEMA
        || payload["source_sha256"] != source_sha256
        || !payload["objects"].is_array()
    {
        return Err(STORAGE);
    }
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_saves_only_a_matching_registered_revision() {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).unwrap();
        let root =
            std::env::temp_dir().join(format!("les-rvt-counts-{:x}", u64::from_le_bytes(random)));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("test.rvt");
        std::fs::write(&path, b"controlled engineering reader input").unwrap();
        let hash = crate::sha256_hex(&std::fs::read(&path).unwrap());
        let db_path = root.join("catalog.sqlite");
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE documents(id TEXT PRIMARY KEY,dataset_id TEXT,file_name TEXT,status TEXT,
                file_hash TEXT,source_path TEXT,file_mtime REAL,file_size INTEGER);
             INSERT INTO documents(id,dataset_id,file_name,status) VALUES('doc','dataset','test.rvt','PENDING');"
        ).unwrap();
        let mut request = json!({"schema":"les.document-convert.v1","path":path,"dataset_id":"dataset",
            "source_sha256":hash,"rvt_count_snapshot":{"database":db_path,
                "file_name":"test.rvt","registration":["doc","",null,null]}});
        let controlled = || {
            json!({"schema":"les.engineering-reader.v1",
            "reader":{"engine":"controlled","revision":"one","format":"rvt"},
            "coverage":{"status":"partial"},"object_count":1,
            "objects":[{"object_id":"element:2","native_id":"2","section":"elements",
                "name":"Fixture","properties":{"rvt_category_id":-2008099},
                "model_context":{"types":[{"native_id":"40","name":"Fixture type"}]}}]})
        };
        // An outdated conversion cannot write the snapshot for a changed registration.
        request["rvt_count_snapshot"]["registration"][1] = json!("previous-location");
        assert_eq!(
            crate::document::conversion::execute_with(&request, &|_| Ok(controlled())).unwrap_err(),
            "INDEX_REGISTRATION_CHANGED"
        );
        request["rvt_count_snapshot"]["registration"][1] = json!("");
        assert!(crate::document::conversion::execute_with(&request, &|_| Ok(controlled())).is_ok());
        let mut selected = json!({"id":"doc","dataset_id":"dataset","file_name":"test.rvt","status":"INDEXED","file_hash":hash});
        assert!(load(&db_path, &selected).unwrap().is_none());
        db.execute(
            "UPDATE documents SET status='INDEXED',file_hash=?1",
            [&hash],
        )
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        let saved = load(&db_path, &selected).unwrap().unwrap();
        assert_eq!(saved["objects"][0]["category_id"], -2008099);
        assert_eq!(
            saved["objects"][0]["model_context"]["types"][0]["native_id"],
            "40"
        );
        selected["dataset_id"] = json!("outside-scope");
        assert!(load(&db_path, &selected).unwrap().is_none());
        selected["dataset_id"] = json!("dataset");
        db.execute("UPDATE documents SET file_hash=?1", ["b".repeat(64)])
            .unwrap();
        assert!(load(&db_path, &selected).unwrap().is_none());
        db.execute("DELETE FROM documents WHERE id='doc'", [])
            .unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM les_rvt_count_snapshots", [], |row| {
                row.get::<_, u32>(0)
            })
            .unwrap(),
            0,
        );
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
