//! Commit an uploaded original and one index job before acknowledging it.
use super::*;
use crate::chat_attachments::{self, UploadClaim};
use rusqlite::{params, TransactionBehavior};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

pub const INDEXED_ATTACHMENT_ROOT: &str = "les:chat-index:v1";
const NAME: &str = "Вложения чата";
const STORE: &str = "ATTACHMENT_STORE_UNAVAILABLE";

fn directory(parent: &Path, name: &str) -> std::result::Result<PathBuf, &'static str> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return Err(STORE),
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| STORE)?;
    if !metadata.is_dir() || !chat_attachments::regular(&path)? {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    if path.canonicalize().map_err(|_| STORE)? != path {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    Ok(path)
}

struct Uncommitted(PathBuf, bool);
impl Drop for Uncommitted {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(&self.0);
            if let Some(parent) = self.0.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }
}

pub fn queue_indexed_attachment(
    database: &Path,
    state: &Path,
    temporary: &Path,
    name: &str,
    claim: &UploadClaim,
) -> std::result::Result<Value, &'static str> {
    if !database.is_absolute()
        || !state.is_absolute()
        || !temporary.is_absolute()
        || name.is_empty()
        || name.ends_with([' ', '.'])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || !library::visible(name)
    {
        return Err("ATTACHMENT_NAME_INVALID");
    }
    let mut conn = connection(database, true)
        .map_err(|_| STORE)?
        .ok_or(STORE)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| STORE)?;
    library::initialize_empty(&tx).map_err(|_| STORE)?;
    intake::init(&tx).map_err(|_| STORE)?;
    let old = first(
        &tx,
        "SELECT a.*,d.file_name FROM les_indexed_attachments a LEFT JOIN documents d
         ON d.id=a.doc_id AND d.dataset_id=a.dataset_id WHERE a.id=?1",
        [&claim.ident],
    )
    .map_err(|_| STORE)?;
    if old.is_object() {
        if claim.key.is_empty() || old["owner"] != claim.owner || old["key_hash"] != claim.key {
            return Err("ATTACHMENT_ID_CONFLICT");
        }
        if old["fingerprint"] != claim.fingerprint {
            return Err("ATTACHMENT_KEY_REUSED");
        }
        if !old["file_name"].is_string() {
            return Err("ATTACHMENT_EXPIRED");
        }
        let source = crate::document_source::resolve(
            "",
            old["dataset_id"].as_str().ok_or(STORE)?,
            old["file_name"].as_str().ok_or(STORE)?,
            &state.join("storage/datasets"),
        )
        .map_err(|_| "ATTACHMENT_EXPIRED")?;
        if chat_attachments::digest(&mut File::open(source).map_err(|_| STORE)?)?.1 != claim.sha256
        {
            return Err("ATTACHMENT_HASH_CHANGED");
        }
        return serde_json::from_str(old["response"].as_str().ok_or(STORE)?).map_err(|_| STORE);
    }
    let mut datasets = rows(
        &tx,
        "SELECT id,coalesce(sensitivity,'') AS sensitivity FROM datasets
         WHERE name=?1 AND coalesce(dataset_scope,'user')='user' LIMIT 2",
        [NAME],
    )
    .map_err(|_| STORE)?;
    let dataset = match datasets.len() {
        0 => {
            let id = "861a7a66-65af-4cd9-8df3-2cd4db6bca70";
            tx.execute(
                "INSERT INTO datasets(id,name,status,sensitivity,dataset_scope)
                VALUES(?1,?2,'IDLE','P0','user')",
                params![id, NAME],
            )
            .map_err(|_| STORE)?;
            json!({"id":id})
        }
        1 if datasets[0]["sensitivity"] == "P0" => datasets.remove(0),
        _ => return Err("ATTACHMENT_INDEX_DATASET_CONFLICT"),
    };
    let dataset = dataset["id"]
        .as_str()
        .filter(|v| {
            !matches!(*v, "" | "." | "..")
                && v.len() <= 160
                && v.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        })
        .ok_or("ATTACHMENT_PATH_UNSAFE")?;
    let active: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM native_intakes WHERE dataset_id=?1 AND finished_at IS NULL
             AND root<>?2)",
            params![dataset, INDEXED_ATTACHMENT_ROOT],
            |row| row.get(0),
        )
        .map_err(|_| STORE)?;
    if active {
        return Err("ATTACHMENT_INDEX_BUSY");
    }
    let root = state.canonicalize().map_err(|_| STORE)?;
    let storage = directory(&root, "storage")?;
    let root = directory(&directory(&storage, "datasets")?, dataset)?;
    let subdir = format!("native-{}", claim.ident);
    let folder = directory(&root, &subdir)?;
    let path = folder.join(name);
    let owned = chat_attachments::regular(&path)?;
    if owned {
        // A crash may leave the file without a committed SQL receipt.
        // Reuse it only when a retry supplies the exact same original.
        if chat_attachments::digest(&mut File::open(&path).map_err(|_| STORE)?)?.1 != claim.sha256 {
            return Err("ATTACHMENT_ID_CONFLICT");
        }
    } else {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(temporary)
            .map_err(|_| STORE)?;
        if chat_attachments::digest(&mut file)? != (claim.size, claim.sha256.clone()) {
            return Err("ATTACHMENT_HASH_CHANGED");
        }
        file.sync_all().map_err(|_| STORE)?;
        drop(file);
        fs::rename(temporary, &path).map_err(|_| STORE)?;
    }
    let mut guard = Uncommitted(path, true);
    let mtime = guard
        .0
        .metadata()
        .and_then(|m| m.modified())
        .map_err(|_| STORE)?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| STORE)?
        .as_secs_f64();
    let id = || {
        tx.query_row("SELECT lower(hex(randomblob(16)))", [], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|_| STORE)
    };
    let job = id()?;
    let doc = id()?;
    let key = format!("{subdir}/{name}");
    let now = intake::timestamp();
    tx.execute(
        "INSERT INTO documents(id,dataset_id,file_name,status,file_size,file_mtime,source_path)
         VALUES(?1,?2,?3,'PENDING',?4,?5,'')",
        params![
            doc,
            dataset,
            key,
            i64::try_from(claim.size).map_err(|_| STORE)?,
            mtime
        ],
    )
    .map_err(|_| STORE)?;
    tx.execute(
        "INSERT INTO native_intakes(id,dataset_id,dataset_name,root,parse_limit,status,phase,
        total,processed,index_pending,message,started_at,updated_at)
        VALUES(?1,?2,?3,?4,1,'queued','index',1,1,1,'Вложение в очереди на индексацию',?5,?5)",
        params![job, dataset, NAME, INDEXED_ATTACHMENT_ROOT, now],
    )
    .map_err(|_| STORE)?;
    tx.execute(
        "INSERT INTO native_intake_files(job_id,ordinal,file_name,source_path) VALUES(?1,0,?2,'')",
        params![job, key],
    )
    .map_err(|_| STORE)?;
    let response = json!({"attachment_id":dataset,"dataset_id":dataset,"dataset_name":NAME,
        "mode":"index","name":name,"doc_id":doc,"intake_job_id":job,"status":"queued"});
    tx.execute("INSERT INTO les_indexed_attachments(id,owner,key_hash,fingerprint,dataset_id,doc_id,job_id,sha256,response)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![claim.ident,claim.owner,claim.key,claim.fingerprint,dataset,doc,job,claim.sha256,response.to_string()])
        .map_err(|_| STORE)?;
    tx.commit().map_err(|_| STORE)?;
    guard.1 = false;
    Ok(response)
}
