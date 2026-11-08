//! Built-in contracts and explicitly enabled MCP tools. Registry reads execute no tools.
use crate::workspace::{Failure, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{path::Path, sync::OnceLock};
fn failed() -> Failure {
    Failure::new(
        "TOOL_REGISTRY_INVALID",
        "Не удалось прочитать описания инструментов. Проверьте подключения MCP.",
    )
}
fn builtins() -> &'static Value {
    static VALUE: OnceLock<Value> = OnceLock::new();
    VALUE.get_or_init(|| {
        serde_json::from_str(include_str!("../../config/builtin-tools.json"))
            .expect("compiled tool contracts")
    })
}
pub fn local_schema(schema: &Value) -> Result<()> {
    let mut pending = vec![schema];
    let mut nodes = 0;
    while let Some(value) = pending.pop() {
        nodes += 1;
        if nodes > 50000 {
            return Err(failed());
        }
        if let Some(map) = value.as_object() {
            for name in ["$ref", "$dynamicRef"] {
                if map
                    .get(name)
                    .is_some_and(|v| !v.as_str().is_some_and(|s| s.starts_with('#')))
                {
                    return Err(failed());
                }
            }
            pending.extend(map.values());
        } else if let Some(array) = value.as_array() {
            pending.extend(array);
        }
    }
    if schema["type"] != "object" || jsonschema::validator_for(schema).is_err() {
        return Err(failed());
    }
    Ok(())
}
pub(crate) fn from_connection(conn: Option<&Connection>, category: Option<&str>) -> Result<Value> {
    let mut result = builtins().clone();
    let tools = result["tools"].as_array_mut().ok_or_else(failed)?;
    if let Some(conn) = conn {
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='les_mcp_connections'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if exists {
            let mut statement =
                conn.prepare("SELECT id,name,tools FROM les_mcp_connections ORDER BY name,id")?;
            let rows = statement.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (id, title, raw) = row?;
                if raw.len() > 4 * 1024 * 1024 {
                    return Err(failed());
                }
                let selected: Value = serde_json::from_str(&raw).map_err(|_| failed())?;
                let selected = selected
                    .as_array()
                    .filter(|v| v.len() <= 32)
                    .ok_or_else(failed)?;
                for tool in selected {
                    if tools.len() >= 4096 || tool["annotations"]["readOnlyHint"] != true {
                        return Err(failed());
                    }
                    let name = tool["name"]
                        .as_str()
                        .filter(|v| !v.is_empty() && v.len() <= 256)
                        .ok_or_else(failed)?;
                    local_schema(&tool["inputSchema"])?;
                    let local_name = format!(
                        "mcp_{}",
                        &crate::sha256_hex(format!("{id}:{name}").as_bytes())[..32]
                    );
                    tools.push(json!({"name":local_name,"version":"1.0.0","title":format!("{title} · {}",tool["title"].as_str().filter(|s|!s.is_empty()).unwrap_or(name)),
                        "category":"mcp","summary":tool["description"].as_str().filter(|s|!s.is_empty()).unwrap_or(name),
                        "input_schema":tool["inputSchema"],"args_schema":tool["inputSchema"],"result_schema":"les_tool_result_v1","returns":"les_tool_result_v1",
                        "effect":"read","side_effects":"none","scopes":["mcp"],"timeout_seconds":30,"retry":"never","idempotency":"none",
                        "result_budget":{"max_chars":16000,"max_items":32},"model_owned_fields":[],"provenance":"Selected external MCP server","approval_required":false,"tags":["mcp"]}));
                }
            }
        }
    }
    if let Some(category) = category.filter(|s| !s.is_empty()) {
        tools.retain(|v| v["category"] == category);
    }
    tools.sort_by(|a, b| {
        [
            a["category"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or(""),
        ]
        .cmp(&[
            b["category"].as_str().unwrap_or(""),
            b["name"].as_str().unwrap_or(""),
        ])
    });
    let count = tools.len();
    result["tool_count"] = json!(count);
    Ok(result)
}
pub fn execute(request: &Value) -> Value {
    let run = || -> Result<Value> {
        if request["schema"] != "les.tool-registry.v1" {
            return Err(failed());
        }
        let database = request["database"].as_str().ok_or_else(failed)?;
        if !Path::new(database).is_absolute() {
            return Err(failed());
        }
        let conn = crate::workspace::connection(Path::new(database), false)?;
        let registry = from_connection(conn.as_ref(), request["category"].as_str())?;
        match request["action"].as_str() {
            Some("registry") => Ok(registry),
            Some("validate") => {
                let tool = registry["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["name"] == request["name"])
                    .ok_or_else(failed)?;
                local_schema(&tool["input_schema"])?;
                let validator =
                    jsonschema::validator_for(&tool["input_schema"]).map_err(|_| failed())?;
                Ok(json!({"valid":validator.is_valid(&request["arguments"])}))
            }
            _ => Err(failed()),
        }
    };
    match run() {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(e) => json!({"status":"failed","code":e.code,"detail":e.detail}),
    }
}
