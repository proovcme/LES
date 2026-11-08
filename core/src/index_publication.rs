//! Shared writer lease and catalog commit for an indexed file generation.
use crate::{files, index_journal, lexical_write};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

const INVALID: &str = "INDEX_PUBLICATION_INVALID";
const STORAGE: &str = "INDEX_CATALOG_WRITE_FAILED";
const CHANGED: &str = "SOURCE_CHANGED_DURING_INDEXING";
const REGISTRATION: &str = "INDEX_REGISTRATION_CHANGED";

pub struct Writer {
    path: PathBuf,
    _lease: File,
}

impl Writer {
    pub fn acquire(content: &Path, collection: &str) -> Result<Self, &'static str> {
        let hash = files::sha256_hex(collection.as_bytes());
        let directory = content.join(".index-replacements").join(&hash[..24]);
        fs::create_dir_all(&directory).map_err(|_| STORAGE)?;
        let lease = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("writer.lock"))
            .map_err(|_| STORAGE)?;
        lease.try_lock().map_err(|_| "INDEX_UPDATE_BUSY")?;
        Ok(Self {
            path: directory.join("journal.sqlite"),
            _lease: lease,
        })
    }

    pub fn journal(&self, action: &str, arguments: Value) -> Result<Value, &'static str> {
        let result = index_journal::execute(
            &json!({"database":self.path,"action":action,"arguments":arguments}),
        )?;
        Ok(result["value"].clone())
    }
}

fn registration(conn: &Connection, request: &Value) -> Result<(), &'static str> {
    let expected = request["registration"]
        .as_array()
        .filter(|row| row.len() == 4)
        .ok_or(INVALID)?;
    let current = conn
        .query_row(
            "SELECT doc.id,coalesce(doc.source_path,''),doc.file_mtime,doc.file_size
         FROM documents doc JOIN datasets ds ON ds.id=doc.dataset_id
         WHERE doc.dataset_id=?1 AND doc.file_name=?2",
            params![
                request["dataset_id"].as_str(),
                request["db_file_name"].as_str()
            ],
            |row| {
                Ok(vec![
                    json!(row.get::<_, String>(0)?),
                    json!(row.get::<_, String>(1)?),
                    json!(row.get::<_, Option<f64>>(2)?),
                    json!(row.get::<_, Option<i64>>(3)?),
                ])
            },
        )
        .optional()
        .map_err(|_| STORAGE)?;
    if !current.is_some_and(|row| {
        row[..2] == expected[..2]
            && row[2].as_f64() == expected[2].as_f64()
            && row[3] == expected[3]
    }) {
        return Err(REGISTRATION);
    }
    // Preserve the stable id while adopting the verified relative search name.
    let name = committed_name(request)?;
    if request["db_file_name"] != name {
        let occupied: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM documents
             WHERE dataset_id=?1 AND file_name=?2 AND id<>?3)",
                params![request["dataset_id"].as_str(), name, expected[0].as_str()],
                |row| row.get(0),
            )
            .map_err(|_| STORAGE)?;
        if occupied {
            return Err("INDEX_SOURCE_ALIAS_CONFLICT");
        }
    }
    Ok(())
}

fn committed_name(request: &Value) -> Result<&str, &'static str> {
    let field = if request["registration"][1] == "" {
        "doc_name"
    } else {
        "db_file_name"
    };
    request[field].as_str().ok_or(INVALID)
}

pub fn check_registration(request: &Value) -> Result<(), &'static str> {
    registration(&lexical_write::open(request)?, request)
}

pub fn check_source(path: &Path, expected: &Value) -> Result<(), &'static str> {
    let actual = files::fingerprint(&json!({"path":path})).map_err(|_| CHANGED)?;
    if actual["sha256"] == expected["file_hash"] && actual["size"] == expected["file_size"] {
        Ok(())
    } else {
        Err(CHANGED)
    }
}

pub fn commit(request: &Value, point_count: usize) -> Result<(), &'static str> {
    let mut conn = lexical_write::open(request)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| STORAGE)?;
    registration(&tx, request)?;
    if lexical_write::replace_in(&tx, request)? != point_count {
        return Err(INVALID);
    }
    let dataset = request["dataset_id"].as_str().ok_or(INVALID)?;
    let name = request["db_file_name"].as_str().ok_or(INVALID)?;
    let fingerprint = &request["fingerprint"];
    let route = &request["route"];
    tx.execute(
        "DELETE FROM structured_rules WHERE file_key=?1",
        [request["doc_name"].as_str()],
    )
    .map_err(|_| STORAGE)?;
    let changed = tx
        .execute(
            "UPDATE documents SET status='INDEXED',chunk_count=?3,last_error='',stage='',
         retryable=0,retry_after=0,error_code='',file_hash=?4,file_mtime=?5,file_size=?6,
         domain=?7,route_dataset=?8,doc_type=?9,content_type=?10,complexity=?11,pipeline=?12,file_name=?13
         WHERE dataset_id=?1 AND file_name=?2",
            params![
                dataset,
                name,
                point_count as i64,
                fingerprint["file_hash"].as_str().ok_or(INVALID)?,
                fingerprint["file_mtime"].as_f64().ok_or(INVALID)?,
                fingerprint["file_size"].as_i64().ok_or(INVALID)?,
                route["domain"].as_str().ok_or(INVALID)?,
                route["dataset_name"].as_str().ok_or(INVALID)?,
                route["doc_type"].as_str().ok_or(INVALID)?,
                route["content_type"].as_str().ok_or(INVALID)?,
                route["complexity"].as_str().ok_or(INVALID)?,
                route["pipeline"].as_str().ok_or(INVALID)?,
                committed_name(request)?
            ],
        )
        .map_err(|_| STORAGE)?;
    if changed != 1 {
        return Err(REGISTRATION);
    }
    crate::tables::artifacts::activate(&tx, request, committed_name(request)?)?;
    update_count(&tx, dataset)?;
    tx.commit().map_err(|_| STORAGE)
}

fn update_count(conn: &Connection, dataset: &str) -> Result<(), &'static str> {
    conn.execute(
        "UPDATE datasets SET chunk_count=(SELECT coalesce(sum(chunk_count),0) FROM documents
                WHERE dataset_id=?1 AND status='INDEXED') WHERE id=?1",
        [dataset],
    )
    .map_err(|_| STORAGE)?;
    Ok(())
}

pub fn check_recovery_registration(request: &Value) -> Result<(), &'static str> {
    recovery_registration(&lexical_write::open(request)?, request)
}

fn recovery_registration(conn: &Connection, request: &Value) -> Result<(), &'static str> {
    let found: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1 AND dataset_id=?2 AND file_name=?3)",
            params![
                request["document_id"].as_str().ok_or(INVALID)?,
                request["dataset_id"].as_str().ok_or(INVALID)?,
                request["db_file_name"].as_str().ok_or(INVALID)?
            ],
            |row| row.get(0),
        )
        .map_err(|_| STORAGE)?;
    if found {
        Ok(())
    } else {
        Err("INDEX_REGISTRATION_CHANGED")
    }
}

pub fn recover(request: &Value) -> Result<(), &'static str> {
    let mut conn = lexical_write::open(request)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| STORAGE)?;
    recovery_registration(&tx, request)?;
    let dataset = request["dataset_id"].as_str().ok_or(INVALID)?;
    let doc = request["document_id"].as_str().ok_or(INVALID)?;
    if request["phase"] == "committing" {
        lexical_write::replace_in(&tx, request)?;
        tx.execute(
            "DELETE FROM structured_rules WHERE file_key=?1",
            [request["doc_name"].as_str()],
        )
        .map_err(|_| STORAGE)?;
    }
    tx.execute("UPDATE documents SET status='PENDING',chunk_count=0,stage='',retryable=0,retry_after=0,
         error_code='INDEX_INTERRUPTED',last_error='Индексация была прервана. Лес повторит обработку файла.'
         WHERE id=?1 AND dataset_id=?2 AND file_name=?3", params![doc,dataset,request["db_file_name"].as_str()])
        .map_err(|_| STORAGE)?;
    update_count(&tx, dataset)?;
    tx.commit().map_err(|_| STORAGE)
}
