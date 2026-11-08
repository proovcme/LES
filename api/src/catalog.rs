//! Native project/note/context CRUD. Model summaries belong to conversation;
//! dossier and note creation side effects still have their explicit legacy owner.
use crate::{error, json_response, Context};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{Method, StatusCode},
    response::Response,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub struct Route {
    action: &'static str,
    id: Option<String>,
    admin: bool,
}
pub fn route(method: &Method, path: &str) -> Option<Route> {
    let (action, id, admin) = if let Some(tail) = path.strip_prefix("/api/projects") {
        let parts = tail.split('/').collect::<Vec<_>>();
        match (method.as_str(), parts.as_slice()) {
            ("GET", [""]) => ("project_list", None, false),
            ("POST", [""]) => ("project_create", None, true),
            ("GET", ["", id]) if !id.is_empty() => ("project_get", Some(*id), false),
            ("PATCH", ["", id]) if !id.is_empty() => ("project_status", Some(*id), true),
            ("DELETE", ["", id]) if !id.is_empty() => ("project_delete", Some(*id), true),
            ("GET", ["", id, "links"]) => ("project_links", Some(*id), false),
            ("POST", ["", id, "links"]) => ("project_link", Some(*id), true),
            ("DELETE", ["", id, "links"]) => ("project_unlink", Some(*id), true),
            _ => return None,
        }
    } else if let Some(tail) = path.strip_prefix("/api/workspace/memory") {
        let parts = tail.split('/').collect::<Vec<_>>();
        match (method.as_str(), parts.as_slice()) {
            ("GET", [""]) => ("note_list", None, false),
            ("PATCH", ["", id]) if !id.is_empty() => ("note_update", Some(*id), false),
            ("DELETE", ["", id]) if !id.is_empty() => ("note_delete", Some(*id), false),
            ("GET", ["", "context", sid]) if !sid.is_empty() => {
                ("context_status", Some(*sid), false)
            }
            ("PATCH", ["", "context", sid]) if !sid.is_empty() => {
                ("context_update", Some(*sid), false)
            }
            ("POST", ["", "context", sid, "forget"]) if !sid.is_empty() => {
                ("context_forget", Some(*sid), false)
            }
            ("GET", ["", "context", sid, "messages"]) if !sid.is_empty() => {
                ("context_exclusions", Some(*sid), false)
            }
            ("PUT", ["", "context", sid, "messages"]) if !sid.is_empty() => {
                ("context_isolate", Some(*sid), false)
            }
            ("GET", ["", "context", sid, "report"]) if !sid.is_empty() => {
                ("context_report", Some(*sid), false)
            }
            _ => return None,
        }
    } else {
        return None;
    };
    let id = match id {
        Some(s) => {
            let decoded = percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .ok()?
                .into_owned();
            if decoded.contains('/') {
                return None;
            }
            Some(decoded)
        }
        None => None,
    };
    Some(Route { action, id, admin })
}

fn invalid(message: &str) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, message)
}
fn query(request: &Request, key: &str) -> Option<String> {
    reqwest::Url::parse(&format!("http://localhost{}", request.uri()))
        .ok()?
        .query_pairs()
        .filter(|(k, _)| k == key)
        .last()
        .map(|(_, v)| v.into_owned())
}
async fn arguments(request: Request, route: &Route) -> Result<Map<String, Value>, Response> {
    let mut args = Map::new();
    let is_context = route.action.starts_with("context_");
    if let Some(id) = &route.id {
        if is_context {
            args.insert("session_id".into(), id.clone().into());
            args.insert("check_session".into(), true.into());
        } else {
            args.insert(
                if route.action.starts_with("project_") {
                    "project_id"
                } else {
                    "note_id"
                }
                .into(),
                id.parse::<i64>()
                    .map_err(|_| invalid("Invalid resource id"))?
                    .into(),
            );
        }
    }
    if matches!(route.action, "note_list" | "note_delete") {
        let project = query(&request, "project_id");
        if route.action == "note_delete" && project.is_none() {
            return Err(invalid("project_id is required"));
        }
        let project = project
            .unwrap_or_else(|| "0".into())
            .parse::<i64>()
            .ok()
            .filter(|id| *id >= 0)
            .ok_or_else(|| invalid("Invalid project_id"))?;
        args.insert("project_id".into(), project.into());
        if route.action == "note_list" {
            args.insert("limit".into(), 500.into());
        }
    }
    if route.action == "project_links" {
        args.insert(
            "kind".into(),
            query(&request, "kind").unwrap_or_default().into(),
        );
    }
    if route.action == "context_report" {
        let id = query(&request, "history_id")
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .ok_or_else(|| invalid("Укажите сообщение для просмотра контекста."))?;
        args.insert("history_id".into(), id.into());
    }
    let needs_body = matches!(
        route.action,
        "project_create"
            | "project_status"
            | "project_link"
            | "project_unlink"
            | "note_update"
            | "context_update"
            | "context_forget"
            | "context_isolate"
    );
    if needs_body {
        let bytes = to_bytes(request.into_body(), 1_048_576)
            .await
            .map_err(|_| {
                error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Запрос слишком велик. Сократите текст.",
                )
            })?;
        let body = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .ok_or_else(|| invalid("Expected a JSON object"))?;
        match route.action {
            "project_create" => {
                let name = body
                    .get("name")
                    .filter(|v| v.is_string())
                    .ok_or_else(|| invalid("name is required"))?;
                args.insert("name".into(), name.clone());
                for key in ["code", "address"] {
                    let value = body
                        .get(key)
                        .filter(|v| !v.is_null())
                        .cloned()
                        .unwrap_or_else(|| json!(""));
                    if !value.is_string() {
                        return Err(invalid("Expected text"));
                    }
                    args.insert(key.into(), value);
                }
            }
            "project_status" => {
                args.insert(
                    "status".into(),
                    body.get("status")
                        .filter(|v| v.is_string())
                        .ok_or_else(|| invalid("status is required"))?
                        .clone(),
                );
            }
            "project_link" | "project_unlink" => {
                for key in ["kind", "ref"] {
                    args.insert(
                        key.into(),
                        body.get(key)
                            .filter(|v| v.is_string())
                            .ok_or_else(|| invalid("kind and ref are required"))?
                            .clone(),
                    );
                }
            }
            "note_update" => {
                let project = body
                    .get("project_id")
                    .and_then(Value::as_i64)
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| invalid("project_id is required"))?;
                args.insert("project_id".into(), project.into());
                args.insert("check_scope".into(), true.into());
                for key in ["text", "enabled"] {
                    if let Some(value) = body.get(key).filter(|v| !v.is_null()) {
                        if (key == "text"
                            && value.as_str().is_none_or(|s| s.chars().count() > 2000))
                            || (key == "enabled" && !value.is_boolean())
                        {
                            return Err(invalid("Invalid note value"));
                        }
                        args.insert(key.into(), value.clone());
                    }
                }
            }
            "context_isolate" => {
                for key in ["history_id", "role", "excluded"] {
                    if let Some(value) = body.get(key) {
                        args.insert(key.into(), value.clone());
                    }
                }
            }
            _ => {
                let revision = body
                    .get("expected_revision")
                    .and_then(Value::as_i64)
                    .filter(|n| *n >= 0)
                    .ok_or_else(|| invalid("expected_revision is required"))?;
                args.insert("expected_revision".into(), revision.into());
                if route.action == "context_update" {
                    let mut changes = Map::new();
                    for key in ["summary", "enabled", "auto_summary"] {
                        if let Some(value) = body.get(key).filter(|v| !v.is_null()) {
                            if key == "summary" {
                                let text = value
                                    .as_str()
                                    .filter(|s| s.chars().count() <= 2200)
                                    .ok_or_else(|| invalid("Invalid summary"))?;
                                let text = text.trim_matches(|c: char| {
                                    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
                                });
                                changes.insert(key.into(), text.into());
                                changes.insert("last_error".into(), "".into());
                                changes.insert("model_revision".into(), "user_edit".into());
                            } else {
                                if !value.is_boolean() {
                                    return Err(invalid("Expected a boolean"));
                                }
                                changes.insert(key.into(), value.clone());
                            }
                        }
                    }
                    args.insert("changes".into(), Value::Object(changes));
                }
            }
        }
    }
    Ok(args)
}

pub async fn handle(context: Arc<Context>, request: Request, route: Route) -> Response {
    let cors = match crate::auth::authorize(&context, request.headers(), route.admin).await {
        Ok(v) => v,
        Err(mut r) => {
            r.headers_mut()
                .insert("x-les-handler", "rust.catalog".parse().unwrap());
            return r;
        }
    };
    let mut response = authorized(context, request, route).await;
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert("x-les-handler", "rust.catalog".parse().unwrap());
    response
}
async fn authorized(context: Arc<Context>, request: Request, route: Route) -> Response {
    let args = match arguments(request, &route).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let permit = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(v)) => v,
        _ => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Хранилище занято. Повторите действие.",
            )
        }
    };
    let database = context.database.clone();
    let action = route.action;
    let result=tokio::task::spawn_blocking(move||{let _permit=permit;les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,"action":action,"arguments":args}))}).await;
    match result {
        Ok(result) if result["status"] == "completed" => {
            let value = result["value"].clone();
            if value.is_null() && matches!(action, "project_get" | "project_status" | "note_update")
            {
                return error(
                    StatusCode::NOT_FOUND,
                    if action == "note_update" {
                        "Заметка не найдена в выбранной области"
                    } else {
                        "Объект не найден"
                    },
                );
            }
            if action == "note_delete" && value == false {
                return error(
                    StatusCode::NOT_FOUND,
                    "Заметка не найдена в выбранной области",
                );
            }
            let value = match action {
                "project_list" => json!({"projects":value}),
                "project_links" => json!({"links":value}),
                "project_delete" | "note_delete" => json!({"deleted":value}),
                "project_unlink" => json!({"unlinked":value}),
                "note_list" => json!({"notes":value}),
                _ => value,
            };
            json_response(StatusCode::OK, value, "rust.catalog")
        }
        Ok(result) => {
            eprintln!(
                "{}",
                json!({"event":"catalog_failed","operation":action,"code":result["code"],"sqlite_extended":result["sqlite_extended"]})
            );
            let status = match result["code"].as_str() {
                Some("WORKSPACE_INVALID") if action.starts_with("project_") => {
                    StatusCode::BAD_REQUEST
                }
                Some("WORKSPACE_INVALID") => StatusCode::UNPROCESSABLE_ENTITY,
                Some("WORKSPACE_CONFLICT") => StatusCode::CONFLICT,
                Some("WORKSPACE_NOT_FOUND") => StatusCode::NOT_FOUND,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            error(
                status,
                result["detail"]
                    .as_str()
                    .unwrap_or("Не удалось прочитать или сохранить данные."),
            )
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось завершить запись. Повторите действие.",
        ),
    }
}
