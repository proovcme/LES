use super::*;
use rusqlite::params;

const KINDS: [&str; 7] = [
    "dataset",
    "cad_bim_import",
    "task",
    "field_zahvatka",
    "mail_thread",
    "note",
    "folder",
];
fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS les_projects(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,code TEXT NOT NULL DEFAULT '',address TEXT NOT NULL DEFAULT '',status TEXT NOT NULL DEFAULT 'active',created_at TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS les_project_links(id INTEGER PRIMARY KEY AUTOINCREMENT,project_id INTEGER NOT NULL,kind TEXT NOT NULL,ref TEXT NOT NULL,created_at TEXT NOT NULL,UNIQUE(project_id,kind,ref));")?;
    Ok(())
}
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
fn links(conn: &Connection, id: i64, kind: &str) -> Result<Value> {
    if !table(conn, "les_project_links")? {
        return Ok(json!([]));
    }
    Ok(if kind.is_empty() {
        json!(rows(
            conn,
            "SELECT * FROM les_project_links WHERE project_id=? ORDER BY kind,id",
            [id]
        )?)
    } else {
        json!(rows(
            conn,
            "SELECT * FROM les_project_links WHERE project_id=? AND kind=? ORDER BY id",
            params![id, kind]
        )?)
    })
}
pub(super) fn get(conn: &Connection, id: i64) -> Result<Value> {
    if !table(conn, "les_projects")? {
        return Ok(Value::Null);
    }
    let mut value = first(conn, "SELECT * FROM les_projects WHERE id=?", [id])?;
    if let Some(object) = value.as_object_mut() {
        object.insert("links".into(), links(conn, id, "")?);
    }
    Ok(value)
}
pub(super) fn datasets(conn: Option<&Connection>, args: &Args) -> Result<Value> {
    let ids = args
        .get("dataset_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::invalid("Invalid dataset_ids"))?;
    if ids.len() > 32766 {
        return Err(Failure::invalid("Too many dataset_ids"));
    }
    let ids = ids
        .iter()
        .map(|v| {
            v.as_str()
                .ok_or_else(|| Failure::invalid("Invalid dataset id"))
        })
        .collect::<Result<Vec<_>>>()?;
    if ids.is_empty() {
        return Ok(json!([]));
    }
    let catalog = match conn {
        Some(conn) if table(conn, "datasets")? => Some(conn),
        _ => None,
    };
    if let Some(conn) = catalog {
        let marks = vec!["?"; ids.len()].join(",");
        let mut values = rows(
            conn,
            &format!("SELECT id,name,chunk_count AS chunks FROM datasets WHERE id IN ({marks})"),
            rusqlite::params_from_iter(&ids),
        )?;
        for value in &mut values {
            let files = if table(conn, "documents")? {
                conn.query_row(
                    "SELECT count(*) FROM documents WHERE dataset_id=?",
                    [value["id"].as_str()],
                    |r| r.get::<_, i64>(0),
                )?
            } else {
                0
            };
            value["files"] = files.into();
        }
        return Ok(json!(values));
    }
    Ok(json!(ids
        .iter()
        .map(|id| json!({"id":id,"name":id,"files":0,"chunks":0}))
        .collect::<Vec<_>>()))
}
pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    let id = integer(args, "project_id", 0)?;
    match action {
        "project_create" => {
            let name = trim(text(args, "name")?);
            if name.is_empty() {
                return Err(Failure::invalid("Пустое имя объекта"));
            }
            let code = trim(args.get("code").and_then(Value::as_str).unwrap_or(""));
            let address = trim(args.get("address").and_then(Value::as_str).unwrap_or(""));
            ensure(conn)?;
            conn.execute("INSERT INTO les_projects(name,code,address,status,created_at) VALUES (?,?,?,'active',?)",params![name,code,address,now()])?;
            get(conn, conn.last_insert_rowid())
        }
        "project_list" => {
            if table(conn, "les_projects")? {
                Ok(json!(rows(
                    conn,
                    "SELECT * FROM les_projects ORDER BY status='archived',id DESC LIMIT ?",
                    [integer(args, "limit", 100)?.min(500)]
                )?))
            } else {
                Ok(json!([]))
            }
        }
        "project_get" => get(conn, id),
        "project_links" => links(
            conn,
            id,
            args.get("kind").and_then(Value::as_str).unwrap_or(""),
        ),
        "project_status" => {
            let status = text(args, "status")?;
            if !["active", "archived", "on_hold"].contains(&status) {
                return Err(Failure::invalid(
                    "status: ['active', 'archived', 'on_hold']",
                ));
            }
            ensure(conn)?;
            conn.execute(
                "UPDATE les_projects SET status=? WHERE id=?",
                params![status, id],
            )?;
            get(conn, id)
        }
        "project_delete" => {
            ensure(conn)?;
            conn.execute("DELETE FROM les_project_links WHERE project_id=?", [id])?;
            Ok(json!(
                conn.execute("DELETE FROM les_projects WHERE id=?", [id])? > 0
            ))
        }
        "project_link" | "project_unlink" => {
            let kind = text(args, "kind")?;
            let reference = trim(text(args, "ref")?);
            if action == "project_link" {
                if !KINDS.contains(&kind) {
                    return Err(Failure::invalid("kind: ['cad_bim_import', 'dataset', 'field_zahvatka', 'folder', 'mail_thread', 'note', 'task']"));
                }
                if reference.is_empty() {
                    return Err(Failure::invalid("Пустой ref"));
                }
                ensure(conn)?;
                if get(conn, id)?.is_null() {
                    return Err(Failure::invalid(&format!("Объект {id} не найден")));
                }
                conn.execute("INSERT OR IGNORE INTO les_project_links(project_id,kind,ref,created_at) VALUES (?,?,?,?)",params![id,kind,reference,now()])?;
                first(
                    conn,
                    "SELECT * FROM les_project_links WHERE project_id=? AND kind=? AND ref=?",
                    params![id, kind, reference],
                )
            } else {
                ensure(conn)?;
                Ok(json!(
                    conn.execute(
                        "DELETE FROM les_project_links WHERE project_id=? AND kind=? AND ref=?",
                        params![id, kind, reference]
                    )? > 0
                ))
            }
        }
        "project_for_dataset" => {
            if !table(conn, "les_project_links")? {
                return Ok(json!(0));
            }
            Ok(json!(conn.query_row("SELECT project_id FROM les_project_links WHERE kind='dataset' AND ref=? ORDER BY id LIMIT 1",[text(args,"dataset_id")?],|r|r.get::<_,i64>(0)).optional()?.unwrap_or(0)))
        }
        "project_datasets" => datasets(Some(conn), args),
        "project_metadata" => {
            let mut meta = Map::new();
            if table(conn, "les_md_context")? {
                for row in rows(conn, "SELECT project_id,meta_json FROM les_md_context", [])? {
                    if let (Some(id), Some(raw)) =
                        (row["project_id"].as_i64(), row["meta_json"].as_str())
                    {
                        if let Ok(value) =
                            serde_json::from_str::<Value>(if raw.is_empty() { "{}" } else { raw })
                        {
                            meta.insert(id.to_string(), value);
                        }
                    }
                }
            }
            Ok(Value::Object(meta))
        }
        _ => Err(Failure::invalid("Unknown project action")),
    }
}
