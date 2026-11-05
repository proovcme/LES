use super::{records, Failure, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

fn trimmed(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn text<'a>(value: &'a Value, name: &str, limit: usize) -> Result<&'a str> {
    value
        .as_str()
        .filter(|text| text.chars().count() <= limit)
        .map(trimmed)
        .ok_or_else(|| Failure::invalid(&format!("Invalid {name}")))
}
fn uuid(value: &str) -> Result<String> {
    let hex = value
        .replace("urn:", "")
        .replace("uuid:", "")
        .trim_matches(['{', '}'])
        .replace('-', "");
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Failure::invalid("Invalid session_id"));
    }
    let hex = hex.to_ascii_lowercase();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}
fn scope(value: &Value) -> Result<Value> {
    let values = value
        .as_object()
        .ok_or_else(|| Failure::invalid("Invalid session scope"))?;
    let mut result = records::empty_scope();
    let allowed = result.as_object().unwrap();
    if values.keys().any(|key| !allowed.contains_key(key)) {
        return Err(Failure::invalid("Invalid session scope"));
    }
    result.as_object_mut().unwrap().extend(values.clone());
    if !matches!(
        result["scope_type"].as_str(),
        Some("none" | "all" | "project" | "projects" | "dataset" | "datasets" | "mixed")
    ) {
        return Err(Failure::invalid("Invalid scope_type"));
    }
    for key in ["project_ids", "dataset_ids"] {
        let items = result[key]
            .as_array()
            .ok_or_else(|| Failure::invalid(&format!("Invalid {key}")))?;
        if items.len() > 500
            || items.iter().any(|item| {
                if key == "project_ids" {
                    item.as_i64().is_none_or(|id| id < 1)
                } else {
                    item.as_str().is_none_or(str::is_empty)
                }
            })
        {
            return Err(Failure::invalid(&format!("Invalid {key}")));
        }
        let mut unique = Vec::new();
        for item in items {
            if !unique.contains(item) {
                unique.push(item.clone());
            }
        }
        result[key] = unique.into();
    }
    if !result["selected_sources_only"].is_boolean() {
        return Err(Failure::invalid("Invalid selected_sources_only"));
    }
    if matches!(result["scope_type"].as_str(), Some("none" | "all"))
        && (!result["project_ids"].as_array().unwrap().is_empty()
            || !result["dataset_ids"].as_array().unwrap().is_empty())
    {
        return Err(Failure::invalid(
            "Scope type conflicts with selected sources",
        ));
    }
    Ok(result)
}
pub(super) fn validate(action: &str, args: &Map<String, Value>) -> Result<()> {
    let allowed = match action {
        "create" => &["project_id", "title", "session_id"][..],
        "update" => &["session_id", "title", "scope", "role"][..],
        _ => &["session_id", "revision", "text", "attachment"][..],
    };
    if args.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(Failure::invalid("Unknown workspace field"));
    }
    if action != "create" {
        records::string(args, "session_id")?;
    }
    if action == "create" {
        records::project(args.get("project_id"))?;
        if let Some(value) = args.get("session_id").filter(|v| !v.is_null()) {
            uuid(
                value
                    .as_str()
                    .ok_or_else(|| Failure::invalid("Invalid session_id"))?,
            )?;
        }
        if let Some(value) = args.get("title") {
            text(value, "title", 200)?;
        }
    } else if action == "update" {
        for (key, limit) in [("title", 200), ("role", 64)] {
            if let Some(value) = args.get(key).filter(|v| !v.is_null()) {
                if text(value, key, limit)?.is_empty() && key == "role" {
                    return Err(Failure::invalid("Empty role"));
                }
            }
        }
        if let Some(value) = args.get("scope").filter(|v| !v.is_null()) {
            scope(value)?;
        }
    } else {
        args.get("revision")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
            .ok_or_else(|| Failure::invalid("Invalid revision"))?;
        // Draft whitespace is evidence and is never trimmed.
        records::string(args, "text")?
            .chars()
            .count()
            .le(&20000)
            .then_some(())
            .ok_or_else(|| Failure::invalid("Invalid text"))?;
        if let Some(value) = args.get("attachment").filter(|v| !v.is_null()) {
            let attachment = value
                .as_object()
                .ok_or_else(|| Failure::invalid("Invalid attachment"))?;
            if attachment.len() != 2 {
                return Err(Failure::invalid("Invalid attachment"));
            }
            let id = records::string(attachment, "attachment_id")?;
            if id.len() != 17
                || !id.starts_with("read_")
                || !id[5..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Failure::invalid("Invalid attachment_id"));
            }
            let name = records::string(attachment, "name")?;
            if name.is_empty() || name.chars().count() > 500 {
                return Err(Failure::invalid("Invalid attachment name"));
            }
        }
    }
    Ok(())
}
pub(super) fn create(conn: &Connection, args: &Map<String, Value>) -> Result<Value> {
    let session_id = if let Some(value) = args.get("session_id").filter(|v| !v.is_null()) {
        uuid(value.as_str().unwrap())?
    } else {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| {
            Failure::new(
                "WORKSPACE_UNAVAILABLE",
                "Система не позволила создать диалог.",
            )
        })?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())?
    };
    if records::get(conn, &session_id)?.is_some() {
        return Err(Failure::conflict("Session already exists"));
    }
    let project = records::project(args.get("project_id"))?;
    let mut selected = records::empty_scope();
    if let Some(id) = project {
        if !records::has_table(conn, "les_projects")?
            || !conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM les_projects WHERE id=?)",
                [id],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Err(Failure::missing("Project not found"));
        }
        if records::has_table(conn, "les_project_links")? {
            let datasets=conn.prepare("SELECT ref FROM les_project_links WHERE project_id=? AND kind='dataset' ORDER BY id")?
                .query_map([id],|row|row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            if !datasets.is_empty() {
                selected = json!({"scope_type":"datasets","project_ids":[],"dataset_ids":datasets,"selected_sources_only":false});
            }
        }
    }
    let now = records::now();
    let result = json!({"session_id":session_id,"project_id":project,"title":trimmed(args.get("title").and_then(Value::as_str).unwrap_or("")),
        "role":"agent","scope":selected,"created_at":now,"updated_at":now,"registered":true});
    records::insert(conn, &result)?;
    Ok(result)
}
pub(super) fn update(conn: &Connection, args: &Map<String, Value>) -> Result<Value> {
    let id = records::string(args, "session_id")?;
    let mut value = records::get(conn, id)?.ok_or_else(|| Failure::missing("Session not found"))?;
    if value["registered"] != true {
        records::insert(conn, &value)?;
    }
    for (name, limit) in [("title", 200), ("role", 64)] {
        if let Some(new) = args.get(name).filter(|v| !v.is_null()) {
            value[name] = text(new, name, limit)?.into();
        }
    }
    if let Some(new) = args.get("scope").filter(|v| !v.is_null()) {
        value["scope"] = scope(new)?;
    }
    value["updated_at"] = records::now().into();
    value["registered"] = true.into();
    conn.execute(
        "UPDATE les_chat_sessions SET title=?,role=?,scope_json=?,updated_at=? WHERE session_id=?",
        params![
            value["title"].as_str(),
            value["role"].as_str(),
            value["scope"].to_string(),
            value["updated_at"].as_str(),
            id
        ],
    )?;
    Ok(value)
}
pub(super) fn save_draft(conn: &Connection, args: &Map<String, Value>) -> Result<Value> {
    let id = records::string(args, "session_id")?;
    let current = records::draft(conn, id)?;
    if current["revision"] != args["revision"] {
        return Err(Failure::conflict("Черновик изменён в другом окне. Ваш текст сохранён в этом окне; откройте диалог заново, чтобы сравнить версии."));
    }
    let revision = args["revision"]
        .as_i64()
        .unwrap()
        .checked_add(1)
        .ok_or_else(|| Failure::invalid("Draft revision limit reached"))?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS les_chat_drafts (session_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,content_json TEXT NOT NULL, updated_at TEXT NOT NULL)")?;
    let content =
        json!({"text":args["text"],"attachment":args.get("attachment").unwrap_or(&Value::Null)});
    conn.execute("INSERT INTO les_chat_drafts VALUES (?,?,?,?) ON CONFLICT(session_id) DO UPDATE SET revision=excluded.revision,content_json=excluded.content_json,updated_at=excluded.updated_at",
        params![id,revision,content.to_string(),records::now()])?;
    Ok(json!({"revision":revision,"text":content["text"],"attachment":content["attachment"]}))
}
