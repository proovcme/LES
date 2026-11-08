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
    let total = conn.query_row(
        "SELECT count(*) FROM documents WHERE dataset_id=?1 AND les_visible_document(file_name)
         AND (?2='' OR les_name_matches(file_name,?2))",
        params![id, q],
        |r| r.get::<_, i64>(0),
    )?;
    let items = rows(
        conn,
        "SELECT id, dataset_id, file_name, COALESCE(status,'') AS status, file_size,
        COALESCE(chunk_count,0) AS chunk_count FROM documents WHERE dataset_id=?1
        AND les_visible_document(file_name) AND (?2='' OR les_name_matches(file_name,?2))
        ORDER BY lower(file_name),id LIMIT ?3 OFFSET ?4",
        params![id, q, limit, offset],
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
    if !table(conn, "datasets")? || !table(conn, "documents")? {
        return Err(Failure::new(
            "WORKSPACE_UNAVAILABLE",
            "Каталог документов ещё запускается.",
        ));
    }
    if action == "dataset_create" {
        return create(conn, args);
    }
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    register_visibility(conn)?;
    conn.create_scalar_function("les_name_matches", 2, flags, |ctx| {
        let name = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        Ok(name.to_lowercase().contains(&ctx.get::<String>(1)?))
    })?;
    match action {
        "library_datasets" => Ok(json!({"datasets":datasets(conn)?})),
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
