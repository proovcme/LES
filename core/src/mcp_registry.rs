//! Existing MCP connection catalog. Network and executable validation belongs to the native MCP controller.
use crate::workspace::{Failure, Result};
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::{json, Value};
use std::path::Path;

fn invalid() -> Failure {
    Failure::new(
        "MCP_CATALOG_INVALID",
        "Не удалось прочитать подключения MCP.",
    )
}
fn open(database: &Path) -> Result<Connection> {
    if !database.is_absolute() {
        return Err(invalid());
    }
    if let Some(parent) = database.parent() {
        std::fs::create_dir_all(parent).map_err(|_| invalid())?;
    }
    let conn = Connection::open(database)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS les_mcp_connections(id TEXT PRIMARY KEY,name TEXT NOT NULL,url TEXT NOT NULL,tools TEXT NOT NULL); CREATE UNIQUE INDEX IF NOT EXISTS les_mcp_unique_url ON les_mcp_connections(url);")?;
    let has_config = conn
        .prepare("PRAGMA table_info(les_mcp_connections)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .any(|r| r.is_ok_and(|v| v == "config"));
    if !has_config {
        conn.execute_batch(
            "ALTER TABLE les_mcp_connections ADD COLUMN config TEXT NOT NULL DEFAULT '{}'",
        )?;
    }
    Ok(conn)
}
pub fn list(database: &Path) -> Result<Vec<Value>> {
    let conn = open(database)?;
    let mut statement =
        conn.prepare("SELECT id,name,url,tools,config FROM les_mcp_connections ORDER BY name,id")?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (id, name, url, tools, config) = row?;
        if tools.len() > 4 * 1024 * 1024 || config.len() > 256000 || result.len() >= 32 {
            return Err(invalid());
        }
        let mut item = json!({"id":id,"name":name,"url":url,"tools":serde_json::from_str::<Value>(&tools).map_err(|_|invalid())?,"transport":"http"});
        let config: Value = serde_json::from_str(&config).map_err(|_| invalid())?;
        if !item["tools"].as_array().is_some_and(|v| v.len() <= 32) {
            return Err(invalid());
        }
        item.as_object_mut().unwrap().extend(
            config
                .as_object()
                .ok_or_else(invalid)?
                .iter()
                .filter(|(key, _)| matches!(key.as_str(), "transport" | "command" | "args"))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        if !matches!(item["transport"].as_str(), Some("http" | "stdio")) {
            return Err(invalid());
        }
        result.push(item);
    }
    Ok(result)
}
pub fn get(database: &Path, id: &str) -> Result<Value> {
    list(database)?
        .into_iter()
        .find(|v| v["id"] == id)
        .ok_or_else(|| {
            Failure::new(
                "MCP_CONNECTION_MISSING",
                "Подключение удалено. Обновите список инструментов",
            )
        })
}
pub fn add(database: &Path, name: &str, url: &str, config: &Value) -> Result<Value> {
    let mut conn = open(database)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tx.query_row("SELECT COUNT(*) FROM les_mcp_connections", [], |r| {
        r.get::<_, i64>(0)
    })? >= 32
    {
        return Err(Failure::new(
            "MCP_CONNECTION_LIMIT",
            "Можно добавить до 32 подключений MCP",
        ));
    }
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM les_mcp_connections WHERE url=?1)",
        [url],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Failure::new(
            "MCP_CONNECTION_DUPLICATE",
            "Этот адрес уже добавлен",
        ));
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| invalid())?;
    let id = bytes.iter().map(|v| format!("{v:02x}")).collect::<String>();
    tx.execute(
        "INSERT INTO les_mcp_connections(id,name,url,tools,config) VALUES(?1,?2,?3,'[]',?4)",
        params![id, name, url, config.to_string()],
    )?;
    tx.commit()?;
    get(database, &id)
}
pub fn remove(database: &Path, id: &str) -> Result<()> {
    open(database)?.execute("DELETE FROM les_mcp_connections WHERE id=?1", [id])?;
    Ok(())
}
pub fn enable(database: &Path, id: &str, tools: &[Value]) -> Result<Value> {
    if tools.len() > 32 {
        return Err(invalid());
    }
    for tool in tools {
        if tool["annotations"]["readOnlyHint"] != true {
            return Err(Failure::new(
                "MCP_TOOL_WRITE_DENIED",
                "Пока доступны только инструменты, объявленные сервером как чтение",
            ));
        }
        crate::tool_registry::local_schema(&tool["inputSchema"])?;
    }
    if open(database)?.execute(
        "UPDATE les_mcp_connections SET tools=?1 WHERE id=?2",
        params![serde_json::to_string(tools).map_err(|_| invalid())?, id],
    )? != 1
    {
        return Err(Failure::new(
            "MCP_CONNECTION_MISSING",
            "Подключение удалено",
        ));
    }
    get(database, id)
}

/// Compatibility identity of existing stdio connections: json.dumps(sort_keys=True), ASCII and spaces.
pub fn stdio_identity(config: &Value) -> String {
    fn string(text: &str) -> String {
        let mut out = String::from("\"");
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                ch if !(32..127).contains(&(ch as u32)) => {
                    for unit in ch.encode_utf16(&mut [0; 2]) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }
    fn encode(value: &Value) -> String {
        match value {
            Value::String(s) => string(s),
            Value::Array(v) => format!("[{}]", v.iter().map(encode).collect::<Vec<_>>().join(", ")),
            Value::Object(v) => {
                let mut pairs = v.iter().collect::<Vec<_>>();
                pairs.sort_by_key(|v| v.0);
                format!(
                    "{{{}}}",
                    pairs
                        .iter()
                        .map(|(k, v)| format!("{}: {}", string(k), encode(v)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            _ => value.to_string(),
        }
    }
    format!("stdio:{}", crate::sha256_hex(encode(config).as_bytes()))
}
