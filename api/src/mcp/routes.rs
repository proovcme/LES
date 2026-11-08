//! Existing MCP settings API, served without the compatibility backend.
use crate::{auth, error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::sync::Arc;
pub struct Route {
    action: &'static str,
    id: Option<String>,
    admin: bool,
}
pub fn route(method: &str, path: &str) -> Option<Route> {
    let parts = path
        .split('/')
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();
    let (action, id) = match (method, parts.as_slice()) {
        ("GET", ["api", "mcp"]) => ("list", None),
        ("POST", ["api", "mcp"]) => ("save", None),
        ("DELETE", ["api", "mcp", id]) => ("remove", Some(*id)),
        ("POST", ["api", "mcp", id, "probe"]) => ("discover", Some(*id)),
        ("PUT", ["api", "mcp", id, "tools"]) => ("enable", Some(*id)),
        _ => return None,
    };
    let id = match id {
        Some(id) => Some(
            percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .ok()?
                .into_owned(),
        ),
        None => None,
    };
    Some(Route {
        action,
        id,
        admin: action != "list",
    })
}
pub async fn handle(context: Arc<Context>, request: Request, route: Route) -> Response {
    let cors = match auth::authorize(&context, request.headers(), route.admin).await {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let mut response = authorized(context, request, route).await;
    response.headers_mut().extend(cors);
    response
}
async fn authorized(context: Arc<Context>, request: Request, route: Route) -> Response {
    let mut input = json!({"schema":"les.mcp.v1","database":context.database,"working_directory":context.state_root,"action":route.action});
    if let Some(id) = route.id {
        if id.is_empty() || id.len() > 256 || id.contains('\0') {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Некорректное подключение MCP",
            );
        }
        input["connection_id"] = id.into();
    }
    if matches!(route.action, "save" | "enable") {
        let bytes = match to_bytes(request.into_body(), 256000).await {
            Ok(bytes) => bytes,
            Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "Запрос MCP слишком большой"),
        };
        let body: Value = match serde_json::from_slice(&bytes) {
            Ok(Value::Object(value)) => value.into(),
            _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Ожидается объект JSON"),
        };
        if route.action == "save" {
            if !body["name"]
                .as_str()
                .is_some_and(|v| !v.is_empty() && v.chars().count() <= 120)
            {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Введите название подключения до 120 символов",
                );
            }
            for (key, default, limit) in [
                ("name", "", 120),
                ("url", "", 2048),
                ("command", "", 4096),
                ("transport", "http", 16),
            ] {
                let value = body.get(key).unwrap_or(&Value::Null);
                let value = if body.get(key).is_none() {
                    default
                } else {
                    match value.as_str() {
                        Some(v) if v.chars().count() <= limit => v,
                        _ => {
                            return error(
                                StatusCode::UNPROCESSABLE_ENTITY,
                                "Некорректные параметры подключения MCP",
                            )
                        }
                    }
                };
                input[key] = value.into();
            }
            if !matches!(input["transport"].as_str(), Some("http" | "stdio")) {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Выберите HTTP или локальную программу",
                );
            }
            input["args"] = body.get("args").cloned().unwrap_or(json!([]));
            if !input["args"]
                .as_array()
                .is_some_and(|v| v.len() <= 32 && v.iter().all(Value::is_string))
            {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Допустимо до 32 строковых аргументов",
                );
            }
        } else {
            input["names"] = body.get("names").cloned().unwrap_or(json!([]));
            if !input["names"]
                .as_array()
                .is_some_and(|v| v.len() <= 32 && v.iter().all(Value::is_string))
            {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Выберите до 32 инструментов",
                );
            }
        }
    }
    let Ok(permit) = context.workspace_slots.clone().try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "MCP_BUSY");
    };
    let runtime = tokio::runtime::Handle::current();
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        runtime.block_on(super::execute(&input))
    })
    .await
    {
        Ok(value) if value["ok"] == true => {
            json_response(StatusCode::OK, value["value"].clone(), "rust.mcp")
        }
        Ok(value) => error(
            StatusCode::CONFLICT,
            value["error"]["message"]
                .as_str()
                .unwrap_or("Не удалось выполнить запрос MCP"),
        ),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "MCP_EXECUTOR_FAILED"),
    }
}
