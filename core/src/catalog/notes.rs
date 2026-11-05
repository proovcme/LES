use super::*;
use rusqlite::params;

fn ensure(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS les_notes(id INTEGER PRIMARY KEY AUTOINCREMENT,text TEXT NOT NULL,dataset_filter TEXT NOT NULL DEFAULT '',created_at REAL NOT NULL);")?;
    let present = columns(conn, "les_notes")?;
    for (name, definition) in [
        ("project_id", "INTEGER NOT NULL DEFAULT 0"),
        ("auto", "INTEGER NOT NULL DEFAULT 0"),
        ("enabled", "INTEGER NOT NULL DEFAULT 1"),
        ("source_session_id", "TEXT"),
    ] {
        if !present.iter().any(|s| s == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE les_notes ADD COLUMN {name} {definition}"
            ))?;
        }
    }
    Ok(())
}
pub(super) fn execute(conn: &Connection, action: &str, args: &Args) -> Result<Value> {
    let project = integer(args, "project_id", 0)?;
    let id = integer(args, "note_id", 0)?;
    if action != "note_projection" {
        ensure(conn)?;
    }
    if boolean(args, "check_scope", false)?
        && project != 0
        && projects::get(conn, project)?.is_null()
    {
        return Err(Failure::missing("Объект не найден"));
    }
    match action {
        "note_create" => {
            let content = trim(text(args, "text")?);
            let filter = args
                .get("dataset_filter")
                .and_then(Value::as_str)
                .unwrap_or("");
            let automatic = boolean(args, "auto", false)?;
            let source = args
                .get("source_session_id")
                .filter(|v| !v.is_null())
                .map(|_| text(args, "source_session_id"))
                .transpose()?;
            let now = seconds();
            conn.execute("INSERT INTO les_notes(text,dataset_filter,project_id,auto,created_at,source_session_id) VALUES (?,?,?,?,?,?)",params![content,filter,project,automatic,now,source])?;
            Ok(
                json!({"id":conn.last_insert_rowid(),"text":content,"dataset_filter":filter,"project_id":project,"created_at":now,"enabled":true,"source_session_id":source}),
            )
        }
        "note_list" => {
            let limit = integer(args, "limit", 50)?;
            Ok(json!(
                if args.get("project_id").is_none_or(Value::is_null) {
                    rows(
                        conn,
                        "SELECT * FROM les_notes ORDER BY id DESC LIMIT ?",
                        [limit],
                    )?
                } else {
                    rows(
                        conn,
                        "SELECT * FROM les_notes WHERE project_id=? ORDER BY id DESC LIMIT ?",
                        params![project, limit],
                    )?
                }
            ))
        }
        "note_projection" => {
            if !table(conn, "les_notes")? {
                return Ok(json!([]));
            }
            let present = columns(conn, "les_notes")?;
            let mut clauses = Vec::new();
            let mut values = Vec::new();
            if present.iter().any(|s| s == "project_id") {
                clauses.push("project_id IN (0,?)");
                values.push(project);
            }
            if present.iter().any(|s| s == "enabled") {
                clauses.push("enabled=1");
            }
            if boolean(args, "explicit_only", false)? && present.iter().any(|s| s == "auto") {
                clauses.push("auto=0");
            }
            let filter = if clauses.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", clauses.join(" AND "))
            };
            values.push(integer(args, "limit", 5)?.max(0));
            Ok(json!(rows(
                conn,
                &format!("SELECT * FROM les_notes{filter} ORDER BY id DESC LIMIT ?"),
                rusqlite::params_from_iter(values)
            )?))
        }
        "note_update" => {
            let content = args
                .get("text")
                .filter(|v| !v.is_null())
                .map(|_| text(args, "text").map(trim))
                .transpose()?;
            if content.is_some_and(|s| s.is_empty() || s.chars().count() > 2000) {
                return Err(Failure::invalid(
                    "Текст заметки должен содержать от 1 до 2000 символов",
                ));
            }
            let enabled = args
                .get("enabled")
                .filter(|v| !v.is_null())
                .map(|_| boolean(args, "enabled", true))
                .transpose()?;
            conn.execute("UPDATE les_notes SET text=COALESCE(?,text),enabled=COALESCE(?,enabled),auto=CASE WHEN ? THEN 0 ELSE auto END WHERE id=? AND project_id=?",params![content,enabled,content.is_some()||enabled.is_some(),id,project])?;
            first(
                conn,
                "SELECT * FROM les_notes WHERE id=? AND project_id=?",
                params![id, project],
            )
        }
        "note_delete" => Ok(json!(
            if args.get("project_id").is_none_or(Value::is_null) {
                conn.execute("DELETE FROM les_notes WHERE id=?", [id])?
            } else {
                conn.execute(
                    "DELETE FROM les_notes WHERE id=? AND project_id=?",
                    params![id, project],
                )?
            } > 0
        )),
        _ => Err(Failure::invalid("Unknown note action")),
    }
}
