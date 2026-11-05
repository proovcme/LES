use crate::{error, json_response, transport, Context};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{Method, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct Route {
    action: &'static str,
    session_id: Option<String>,
}
pub fn route(method: &Method, path: &str) -> Option<Route> {
    let tail = path.strip_prefix("/api/workspace/sessions")?;
    let parts: Vec<_> = tail.split('/').collect();
    let (action, sid) = match (method.as_str(), parts.as_slice()) {
        ("GET", [""]) => ("list", None),
        ("POST", [""]) => ("create", None),
        ("GET", ["", sid]) if !sid.is_empty() => ("get", Some(*sid)),
        ("PATCH", ["", sid]) if !sid.is_empty() => ("update", Some(*sid)),
        ("GET", ["", sid, "draft"]) if !sid.is_empty() => ("draft_get", Some(*sid)),
        ("PUT", ["", sid, "draft"]) if !sid.is_empty() => ("draft_save", Some(*sid)),
        _ => return None,
    };
    let session_id = match sid {
        Some(sid) => {
            let sid = percent_encoding::percent_decode_str(sid)
                .decode_utf8()
                .ok()?
                .into_owned();
            if sid.contains('/') {
                return None;
            }
            Some(sid)
        }
        None => None,
    };
    Some(Route { action, session_id })
}

pub async fn handle(context: Arc<Context>, request: Request, route: Route) -> Response {
    let cors = match transport::authorize(&context, request.headers()).await {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let mut response = authorized(context, request, route).await;
    response.headers_mut().extend(cors);
    response
}

async fn authorized(context: Arc<Context>, request: Request, route: Route) -> Response {
    let mut arguments = serde_json::Map::new();
    if route.action == "list" {
        let query = reqwest::Url::parse(&format!("http://localhost{}", request.uri()))
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .filter(|(name, _)| name == "project_id")
                    .map(|(_, value)| value.into_owned())
                    .last()
            });
        if let Some(value) = query {
            let Some(id) = value.parse::<i64>().ok().filter(|id| *id > 0) else {
                return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid project_id");
            };
            arguments.insert("project_id".into(), id.into());
        }
    } else if matches!(route.action, "create" | "update" | "draft_save") {
        let bytes = match to_bytes(request.into_body(), 1_048_576).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Запрос слишком велик. Сократите текст и повторите действие.",
                )
            }
        };
        arguments = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(value)) => value,
            _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Expected a JSON object"),
        };
        if arguments.contains_key("session_id") && route.session_id.is_some() {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "Unknown workspace field");
        }
    }
    if let Some(id) = route.session_id {
        arguments.insert("session_id".into(), id.into());
    }
    let database = context.database.clone();
    let permit = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Хранилище диалогов занято. Текст остаётся в этом окне; повторите действие.",
            )
        }
    };
    let result=tokio::task::spawn_blocking(move|| {
        // Keep the slot until the transaction ends, even if the HTTP caller disconnects.
        let _permit=permit;
        les_core::workspace::execute(&json!({"schema":"les.workspace.v1","database":database,"action":route.action,"arguments":arguments}))
    }).await;
    match result {
        Ok(result) if result["status"] == "completed" => {
            let value = if route.action == "list" {
                json!({"sessions":result["value"]})
            } else {
                result["value"].clone()
            };
            if route.action == "get" && value.is_null() {
                error(StatusCode::NOT_FOUND, "Session not found")
            } else {
                json_response(StatusCode::OK, value, "rust.workspace")
            }
        }
        Ok(result) => {
            eprintln!(
                "{}",
                json!({"event":"workspace_failed","operation":route.action,
                "code":result["code"],"sqlite_extended":result["sqlite_extended"]})
            );
            let status = match result["code"].as_str() {
                Some("WORKSPACE_INVALID") => StatusCode::UNPROCESSABLE_ENTITY,
                Some("WORKSPACE_CONFLICT") => StatusCode::CONFLICT,
                Some("WORKSPACE_NOT_FOUND") => StatusCode::NOT_FOUND,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            error(
                status,
                result["detail"]
                    .as_str()
                    .unwrap_or("Не удалось сохранить диалог. Текст остаётся в этом окне."),
            )
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось завершить запись диалога. Текст остаётся в этом окне; повторите действие.",
        ),
    }
}
