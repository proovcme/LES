//! First-index identity and crash-safe publication, under the normal writer lease.
use crate::index_publication::Writer;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const STORAGE: &str = "INDEX_SETUP_STORAGE_UNAVAILABLE";
pub fn read(path: &Path) -> Result<Option<Value>, &'static str> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(STORAGE),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| STORAGE)?;
    if bytes.len() > 1024 * 1024 {
        return Err("INDEX_CONTRACT_INVALID");
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "INDEX_CONTRACT_INVALID")?;
    if !value.is_object() {
        return Err("INDEX_CONTRACT_INVALID");
    }
    Ok(Some(value))
}

pub fn model_key(value: &str) -> String {
    let value = value.trim().to_lowercase();
    let value = value.strip_suffix(":latest").unwrap_or(&value);
    match value {
        "baai/bge-m3" => "bge-m3".into(),
        "qwen/qwen3-embedding-0.6b" => "qwen3-embedding-0.6b".into(),
        "qwen/qwen3-embedding-4b" => "qwen3-embedding-4b".into(),
        "qwen/qwen3-embedding-8b" => "qwen3-embedding-8b".into(),
        _ => value.into(),
    }
}

/// The selected role is mandatory; defaults describe algorithms, never a fallback model.
pub fn contract(
    settings: &Value,
    model: &str,
    revision: &str,
    dimension: u64,
    backend: &str,
    tokenizer: Value,
) -> Result<Value, &'static str> {
    if model.is_empty()
        || revision.is_empty()
        || revision.len() > 256
        || !(1..=65536).contains(&dimension)
    {
        return Err("EMBEDDING_DESCRIPTOR_INVALID");
    }
    let get = |key: &str, default: &str| settings[key].as_str().unwrap_or(default).to_owned();
    let profile = get("LES_EMBED_PROFILE", "legacy").to_lowercase();
    let (default_model, collection, size, overlap) = match profile.as_str() {
        "legacy" => ("BAAI/bge-m3", "les_rag", 900, 80),
        "quality" => ("BAAI/bge-m3", "les_rag_bge_m3", 900, 80),
        "qwen" => ("Qwen/Qwen3-Embedding-0.6B", "les_rag", 1550, 70),
        "fast" => ("intfloat/multilingual-e5-small", "les_rag_fast", 1200, 80),
        _ => return Err("INDEX_SETTINGS_INVALID"),
    };
    let configured = get("EMBEDDING_MODEL", &get("BGE_MODEL", default_model));
    if settings["EMBEDDING_MODEL"]
        .as_str()
        .is_some_and(|v| !v.is_empty())
        && model_key(&configured) != model_key(model)
    {
        return Err("EMBEDDING_INDEX_MODEL_MISMATCH");
    }
    let canonical = if model_key(&configured) == model_key(model) {
        configured
    } else {
        model.into()
    };
    let number = |key: &str, default: u64| -> Result<u64, &'static str> {
        settings[key].as_str().map_or(Ok(default), |v| {
            v.parse().map_err(|_| "INDEX_SETTINGS_INVALID")
        })
    };
    if number("RAG_VECTOR_SIZE", dimension)? != dimension {
        return Err("EMBEDDING_INDEX_DIMENSION_MISMATCH");
    }
    let unit = get("RAG_CHUNK_UNIT", "tokens");
    let (size, overlap) = if unit == "tokens" {
        let mut size = number("RAG_CHUNK_TOKENS", 430)?;
        let overlap = number("RAG_CHUNK_OVERLAP_TOKENS", 50)?;
        let budget = number("COREML_EMBED_SEQ_LEN", 512)?
            .checked_sub(32)
            .ok_or("INDEX_SETTINGS_INVALID")?;
        if size.saturating_add(overlap) > budget {
            size = budget.saturating_sub(overlap).max(64);
        }
        (size, overlap)
    } else if unit == "chars" {
        (
            number("RAG_CHUNK_SIZE", size)?,
            number("RAG_CHUNK_OVERLAP", overlap)?,
        )
    } else {
        return Err("INDEX_SETTINGS_INVALID");
    };
    let backend = get("EMBED_BACKEND", backend);
    let mut descriptor = json!({"backend":backend,"model_id":canonical,"profile":profile,"vector_size":dimension.to_string()});
    if backend == "coreml" {
        for (key, env) in [
            ("coreml_model", "COREML_EMBED_MODEL"),
            ("coreml_seq_len", "COREML_EMBED_SEQ_LEN"),
            ("coreml_compute_units", "COREML_EMBED_COMPUTE_UNITS"),
            ("coreml_fallback", "COREML_EMBED_FALLBACK"),
        ] {
            descriptor[key] = json!(get(env, ""));
        }
    }
    use sha1::{Digest, Sha1};
    let mut fields = descriptor
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
        .collect::<Vec<_>>();
    fields.sort_unstable();
    let mut result = json!({"schema":"les.rag.index-contract.v3","collection":get("RAG_COLLECTION_NAME",collection),
        "embedding_model":canonical,"embedding_api_model":model,"embedding_backend":backend,"vector_size":dimension,
        "document_embedding_mode":"raw-v1","chunk_unit":unit,"chunk_size":size,"chunk_overlap":overlap,
        "chunker_revision":"structure-aware-final-budget-v2","qdrant_schema":"named","dense_vector_name":get("RAG_DENSE_VECTOR_NAME","dense"),
        "sparse_vector_name":get("RAG_SPARSE_VECTOR_NAME","bm25_sparse"),"sparse_tokenizer_revision":get("RAG_SPARSE_TOKENIZER_REVISION","les-bm25-v1"),
        "point_embedding_fingerprint":format!("{:x}",Sha1::digest(fields.join("\n").as_bytes())),
        "hierarchy_schema":"les.rag.hierarchy.v1","hierarchy_builder":"deterministic-heading-stack-v1",
        "hierarchy_retrieval":"soft-global-plus-descendant-rrf-v1","navigation_evidence_policy":"navigation_not_evidence",
        "native_embedding_revision":revision});
    if unit == "tokens" {
        let info =
            crate::document::execute(&json!({"action":"tokenizer_info","tokenizer":tokenizer}))?;
        result["chunk_tokenizer_sha256"] = info["sha256"].clone();
    }
    if backend == "coreml" {
        for (key, env) in [
            ("embedding_package", "COREML_EMBED_MODEL"),
            ("embedding_compute_units", "COREML_EMBED_COMPUTE_UNITS"),
            ("embedding_fallback", "COREML_EMBED_FALLBACK"),
        ] {
            result[key] = json!(get(env, ""));
        }
        result["embedding_seq_len"] = json!(number("COREML_EMBED_SEQ_LEN", 512)?);
    }
    result.sort_all_objects();
    result["fingerprint"] = json!(crate::sha256_hex(result.to_string().as_bytes()));
    crate::index_configuration::execute(
        &json!({"contract":result,"settings":settings,"tokenizer":tokenizer}),
    )?;
    Ok(result)
}

/// Shared atomic sidecar primitive. A caller must separately establish model and
/// collection ownership; this does not initialize or adopt a vector index.
pub fn publish(path: &Path, contract: &Value) -> Result<(), &'static str> {
    if !path.is_absolute() || contract["schema"] != "les.rag.index-contract.v3" {
        return Err("INDEX_CONTRACT_INVALID");
    }
    let mut unsigned = contract.clone();
    unsigned
        .as_object_mut()
        .ok_or("INDEX_CONTRACT_INVALID")?
        .remove("fingerprint");
    unsigned.sort_all_objects();
    let encoded = unsigned.to_string();
    if encoded.len() > 1024 * 1024
        || contract["fingerprint"].as_str() != Some(&crate::sha256_hex(encoded.as_bytes()))
    {
        return Err("INDEX_CONTRACT_INVALID");
    }
    if path.try_exists().map_err(|_| STORAGE)? {
        return Err("INDEX_CONTRACT_EXISTS");
    }
    let parent = path.parent().ok_or(STORAGE)?;
    fs::create_dir_all(parent).map_err(|_| STORAGE)?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| STORAGE)?;
    let temporary = parent.join(format!(
        ".index-contract-{}.tmp",
        crate::sha256_hex(&random)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| STORAGE)?;
    let result: Result<(), &'static str> = (|| {
        file.write_all((contract.to_string() + "\n").as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| STORAGE)?;
        drop(file);
        fs::hard_link(&temporary, path).map_err(|_| "INDEX_CONTRACT_PUBLICATION_FAILED")?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    publish(
        Path::new(request["path"].as_str().ok_or("INDEX_CONTRACT_INVALID")?),
        &request["contract"],
    )?;
    Ok(json!({"status":"published"}))
}

pub struct Session {
    _writer: Writer,
    conn: Connection,
    collection: String,
    path: PathBuf,
}
pub fn status(database: &Path, collection: &str) -> Result<Value, &'static str> {
    if !database.exists() {
        return Ok(json!({"phase":"not_started"}));
    }
    let conn = Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| STORAGE)?;
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(|_| STORAGE)?;
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='les_index_setup')",[],|r|r.get(0)).map_err(|_|STORAGE)?;
    if !exists {
        return Ok(json!({"phase":"not_started"}));
    }
    conn.query_row("SELECT phase,model_revision,error_code,updated_at FROM les_index_setup WHERE collection=?1",[collection],|r|Ok(json!({"phase":r.get::<_,String>(0)?,"model_revision":r.get::<_,String>(1)?,"error_code":r.get::<_,Option<String>>(2)?,"updated_at":r.get::<_,String>(3)?})))
        .optional().map(|v|v.unwrap_or(json!({"phase":"not_started"}))).map_err(|_|STORAGE)
}
impl Session {
    pub fn writer(&self) -> &Writer {
        &self._writer
    }
    pub fn open(
        database: &Path,
        content: &Path,
        collection: &str,
        path: &Path,
    ) -> Result<Self, &'static str> {
        if !path.is_absolute()
            || matches!(collection, "" | "." | "..")
            || collection.len() > 255
            || collection
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err("INDEX_SETTINGS_INVALID");
        }
        let writer = Writer::acquire(content, collection)?;
        let journal = writer.journal("read", json!({}))?;
        if !journal.is_null() && journal["phase"] != "idle" {
            return Err("INDEX_RECOVERY_INCOMPLETE");
        }
        let conn = Connection::open(database).map_err(|_| STORAGE)?;
        conn.busy_timeout(Duration::from_secs(3))
            .map_err(|_| STORAGE)?;
        conn.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS les_index_setup(collection TEXT PRIMARY KEY,contract TEXT NOT NULL,model_revision TEXT NOT NULL,phase TEXT NOT NULL,error_code TEXT,updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);").map_err(|_| STORAGE)?;
        conn.execute("INSERT OR IGNORE INTO les_index_setup(collection,contract,model_revision,phase) VALUES(?1,'{}','','waiting')",[collection]).map_err(|_|STORAGE)?;
        Ok(Self {
            _writer: writer,
            conn,
            collection: collection.into(),
            path: path.into(),
        })
    }
    pub fn pending(&self) -> Result<Option<Value>, &'static str> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT contract FROM les_index_setup WHERE collection=?1 AND phase='planned'",
                [&self.collection],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| STORAGE)?;
        text.map(|s| serde_json::from_str(&s).map_err(|_| STORAGE))
            .transpose()
    }
    pub fn indexed_count(&self) -> Result<u64, &'static str> {
        let exists: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='documents')",[],|r|r.get(0)).map_err(|_| STORAGE)?;
        if !exists {
            return Ok(0);
        }
        let count:i64=self.conn.query_row("SELECT coalesce(sum(chunk_count),0) FROM documents WHERE status='INDEXED' AND chunk_count>0",[],|r|r.get(0)).map_err(|_| "INDEX_CATALOG_VERIFICATION_UNAVAILABLE")?;
        count
            .try_into()
            .map_err(|_| "INDEX_CATALOG_VERIFICATION_UNAVAILABLE")
    }
    pub fn plan(&self, contract: &Value, revision: &str) -> Result<(), &'static str> {
        if let Some(saved) = self.pending()? {
            if saved != *contract {
                return Err("INDEX_SETUP_PLAN_CHANGED");
            }
        }
        self.conn.execute("INSERT INTO les_index_setup(collection,contract,model_revision,phase) VALUES(?1,?2,?3,'planned') ON CONFLICT(collection) DO UPDATE SET contract=excluded.contract,model_revision=excluded.model_revision,phase='planned',error_code=NULL,updated_at=CURRENT_TIMESTAMP",rusqlite::params![self.collection,contract.to_string(),revision]).map_err(|_| STORAGE)?;
        Ok(())
    }
    pub fn probe(&self, revision: &str) -> Result<(), &'static str> {
        self.conn.execute("UPDATE les_index_setup SET phase='probing',model_revision=?2,error_code=NULL,updated_at=CURRENT_TIMESTAMP WHERE collection=?1 AND phase<>'planned'",rusqlite::params![self.collection,revision]).map_err(|_|STORAGE)?;
        Ok(())
    }
    pub fn finish(&self, contract: &Value) -> Result<(), &'static str> {
        match read(&self.path)? {
            Some(saved) if saved != *contract => return Err("INDEX_CONTRACT_MISMATCH"),
            Some(_) => (),
            None => publish(&self.path, contract)?,
        }
        self.conn.execute("UPDATE les_index_setup SET phase='completed',error_code=NULL,updated_at=CURRENT_TIMESTAMP WHERE collection=?1",[&self.collection]).map_err(|_| STORAGE)?;
        Ok(())
    }
    pub fn failed(&self, code: &str) {
        let key = code.split(':').next().unwrap_or(code);
        let safe = if key == "UPSTREAM_HTTP_ERROR" {
            code.split_once(':')
                .and_then(|(_, v)| v.trim().parse::<u16>().ok())
                .filter(|n| (100..=599).contains(n))
                .map(|n| format!("{key}:{n}"))
                .unwrap_or_else(|| key.into())
        } else {
            key.into()
        };
        let _ = self.conn.execute("UPDATE les_index_setup SET error_code=?2,phase=CASE WHEN phase='planned' THEN 'planned' ELSE 'blocked' END,updated_at=CURRENT_TIMESTAMP WHERE collection=?1",rusqlite::params![self.collection,safe]);
    }
}
