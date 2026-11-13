//! User-visible document inventory. No source file I/O, models, or index mutation.
use super::{integer, rows, table, text, trim, Args};
use crate::workspace::{Failure, Result};
use rusqlite::{functions::FunctionFlags, params, Connection};
use serde_json::{json, Value};

pub(crate) fn visible(name: &str) -> bool {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or_default();
    !["~$", ".~lock.", "._"]
        .iter()
        .any(|prefix| leaf.starts_with(prefix))
        && !matches!(
            leaf,
            ".pdf_preprocess_state.json" | "_les_dataset_profile.json"
        )
}

pub(crate) fn register_visibility(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "les_visible_document",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            Ok(ctx
                .get::<Option<String>>(0)?
                .is_some_and(|name| visible(&name)))
        },
    )
}

pub(super) fn initialize_empty(conn: &Connection) -> Result<()> {
    match (table(conn, "datasets")?, table(conn, "documents")?) {
        (true, true) => return Ok(()),
        (false, false) => (),
        _ => {
            return Err(Failure::new(
                "WORKSPACE_UNAVAILABLE",
                "Каталог документов неполон.",
            ))
        }
    }
    conn.execute_batch(
        "CREATE TABLE datasets(id TEXT PRIMARY KEY,name TEXT,status TEXT,chunk_count INTEGER DEFAULT 0,
            sensitivity TEXT DEFAULT 'P0',group_name TEXT DEFAULT '',dataset_scope TEXT DEFAULT 'user',
            module_id TEXT DEFAULT '');
         CREATE TABLE documents(id TEXT PRIMARY KEY,dataset_id TEXT,file_name TEXT,status TEXT,file_hash TEXT,
            file_mtime REAL,file_size INTEGER,chunk_count INTEGER DEFAULT 0,domain TEXT DEFAULT '',
            route_dataset TEXT DEFAULT '',doc_type TEXT DEFAULT '',content_type TEXT DEFAULT '',
            complexity TEXT DEFAULT '',pipeline TEXT DEFAULT '',last_error TEXT DEFAULT '',
            stage TEXT DEFAULT '',source_path TEXT DEFAULT '',parse_attempts INTEGER DEFAULT 0,
            last_attempt_at REAL DEFAULT 0,retryable INTEGER DEFAULT 0,retry_after REAL DEFAULT 0,
            error_code TEXT DEFAULT '');
         CREATE INDEX idx_docs_dataset ON documents(dataset_id);
         CREATE INDEX idx_docs_source ON documents(dataset_id,file_name);
         CREATE TABLE structured_rules(id TEXT PRIMARY KEY,document_id TEXT NOT NULL,file_key TEXT NOT NULL,
            chunk_id TEXT NOT NULL,subject TEXT NOT NULL,parameter TEXT NOT NULL,operator TEXT NOT NULL,
            value REAL NOT NULL,unit TEXT NOT NULL,condition TEXT,char_start INTEGER NOT NULL,char_end INTEGER NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP);
         CREATE INDEX idx_rules_doc ON structured_rules(document_id);
         CREATE INDEX idx_rules_file ON structured_rules(file_key);",
    )?;
    Ok(())
}

fn create(conn: &Connection, args: &Args) -> Result<Value> {
    let name = trim(text(args, "name")?);
    if name.is_empty() {
        return Err(Failure::invalid("Введите название датасета."));
    }
    if name.chars().count() > 120 {
        return Err(Failure::invalid(
            "Название датасета должно содержать не более 120 символов.",
        ));
    }
    if name.chars().any(|c| c < ' ' || c == '\u{7f}') {
        return Err(Failure::invalid(
            "Название датасета не должно содержать управляющие символы.",
        ));
    }
    // Use the same random UUID layout as existing dataset identities.
    let id: String = conn.query_row(
        "SELECT lower(hex(randomblob(4)) || '-' || hex(randomblob(2)) || '-4' ||
         substr(hex(randomblob(2)),2) || '-' ||
         substr('89ab', (random() & 3) + 1, 1) || substr(hex(randomblob(2)),2) || '-' ||
         hex(randomblob(6)))",
        [],
        |r| r.get(0),
    )?;
    conn.execute("INSERT INTO datasets(id,name,status,dataset_scope,module_id) VALUES(?1,?2,'IDLE','user','')",
        params![id, name])?;
    Ok(json!({"id":id,"name":name}))
}

fn datasets(conn: &Connection) -> Result<Vec<Value>> {
    rows(conn, "SELECT d.id, COALESCE(d.name,d.id) AS name, COALESCE(d.status,'') AS status,
        COALESCE(d.chunk_count,0) AS chunk_count, COUNT(doc.id) AS document_count
        FROM datasets d LEFT JOIN documents doc ON doc.dataset_id=d.id AND les_visible_document(doc.file_name)
        WHERE COALESCE(d.dataset_scope,'user') <> 'system'
        GROUP BY d.id ORDER BY lower(d.name),d.id", [])
}

fn check_dataset(conn: &Connection, id: &str) -> Result<()> {
    if conn.query_row("SELECT EXISTS(SELECT 1 FROM datasets WHERE id=?1 AND COALESCE(dataset_scope,'user') <> 'system')",
        [id], |r| r.get::<_, bool>(0))? {
        Ok(())
    } else {
        Err(Failure::missing("Набор документов не найден."))
    }
}

fn documents(conn: &Connection, args: &Args) -> Result<Value> {
    let id = text(args, "dataset_id")?;
    let limit = integer(args, "limit", 50)?;
    let offset = integer(args, "offset", 0)?;
    if !(1..=100).contains(&limit) || offset < 0 {
        return Err(Failure::invalid(
            "Размер страницы от 1 до 100; смещение от 0.",
        ));
    }
    check_dataset(conn, id)?;
    let q = trim(args.get("q").and_then(Value::as_str).unwrap_or_default()).to_lowercase();
    let file_filter = args.get("doc_filter").cloned().unwrap_or(json!([]));
    if !file_filter
        .as_array()
        .is_some_and(|a| a.len() <= 2048 && a.iter().all(Value::is_string))
    {
        return Err(Failure::invalid("Некорректный выбор документов."));
    }
    let file_filter = file_filter.to_string();
    let total = conn.query_row(
        "SELECT count(*) FROM documents WHERE dataset_id=?1 AND les_visible_document(file_name)
         AND (?2='' OR les_name_matches(file_name,?2))
         AND (?3='[]' OR file_name IN (SELECT value FROM json_each(?3)))",
        params![id, q, file_filter],
        |r| r.get::<_, i64>(0),
    )?;
    let items = rows(
        conn,
        "SELECT id, dataset_id, file_name, COALESCE(status,'') AS status, file_size,
        COALESCE(chunk_count,0) AS chunk_count FROM documents WHERE dataset_id=?1
        AND les_visible_document(file_name) AND (?2='' OR les_name_matches(file_name,?2))
        AND (?5='[]' OR file_name IN (SELECT value FROM json_each(?5)))
        ORDER BY lower(file_name),id LIMIT ?3 OFFSET ?4",
        params![id, q, limit, offset, file_filter],
    )?;
    Ok(json!({"dataset_id":id,"limit":limit,"offset":offset,"total":total,"documents":items}))
}

fn forest(conn: &Connection) -> Result<Value> {
    let mut nodes = rows(
        conn,
        "SELECT 'ds:'||id AS id, 'dataset' AS kind, COALESCE(name,id) AS label,
        COALESCE(chunk_count,0) AS chunks FROM datasets
        WHERE COALESCE(dataset_scope,'user') <> 'system' ORDER BY lower(name),id",
        [],
    )?;
    nodes.extend(rows(conn, "SELECT doc.dataset_id||':'||doc.file_name AS id, 'document' AS kind,
        doc.file_name AS label, doc.id AS document_id, doc.dataset_id, COALESCE(d.name,d.id) AS dataset,
        COALESCE(doc.chunk_count,0) AS chunks, COALESCE(doc.status,'') AS status, doc.file_size
        FROM documents doc JOIN datasets d ON d.id=doc.dataset_id
        WHERE COALESCE(d.dataset_scope,'user') <> 'system' AND les_visible_document(doc.file_name)
        ORDER BY doc.dataset_id,doc.file_name,doc.id", [])?);
    Ok(json!({"nodes":nodes}))
}

pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    if action == "dataset_create" {
        initialize_empty(conn)?;
        return create(conn, args);
    }
    let has_datasets = table(conn, "datasets")?;
    let has_documents = table(conn, "documents")?;
    if action == "library_datasets" && !has_datasets && !has_documents {
        return Ok(json!({"datasets":[]}));
    }
    if !has_datasets || !has_documents {
        return Err(Failure::new(
            "WORKSPACE_UNAVAILABLE",
            "Каталог документов ещё запускается.",
        ));
    }
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    register_visibility(conn)?;
    conn.create_scalar_function("les_name_matches", 2, flags, |ctx| {
        let name = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        Ok(name.to_lowercase().contains(&ctx.get::<String>(1)?))
    })?;
    match action {
        "library_datasets" => Ok(json!({"datasets":datasets(conn)?})),
        "library_document" => {
            let id = args.get("doc_id").and_then(Value::as_str).unwrap_or("");
            let dataset = args.get("dataset_id").and_then(Value::as_str).unwrap_or("");
            let name = args.get("file_name").and_then(Value::as_str).unwrap_or("");
            if id.is_empty() && (dataset.is_empty() || name.is_empty()) {
                return Err(Failure::invalid("Document identity required"));
            }
            let records=rows(conn,"SELECT doc.* FROM documents doc JOIN datasets d ON doc.dataset_id=d.id WHERE (?1='' OR doc.id=?1) AND (?2='' OR doc.dataset_id=?2) AND (?3='' OR doc.file_name=?3) AND COALESCE(d.dataset_scope,'user') <> 'system' AND les_visible_document(doc.file_name) LIMIT 2",params![id,dataset,name])?;
            if records.len() > 1 {
                return Err(Failure::new(
                    "DOCUMENT_IDENTITY_AMBIGUOUS",
                    "Найдено несколько редакций документа. Выберите точный источник.",
                ));
            }
            Ok(records.into_iter().next().unwrap_or(Value::Null))
        }
        "library_dataset" => {
            let id = text(args, "dataset_id")?;
            check_dataset(conn, id)?;
            Ok(json!({"dataset_id":id}))
        }
        "library_documents" => documents(conn, args),
        "library_forest" => forest(conn),
        _ => Err(Failure::invalid("Unknown library action")),
    }
}
