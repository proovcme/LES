//! Public profile paths use the existing SQLite snapshots, without a Python API hop.
use crate::{auth, error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::sync::Arc;
pub struct Route {
    action: &'static str,
    arguments: Value,
    admin: bool,
}
pub fn route(method: &str, path: &str) -> Option<Route> {
    if method == "GET" && path == "/api/tools/registry" {
        return Some(Route {
            action: "tools",
            arguments: json!({}),
            admin: false,
        });
    }
    let tail = path.strip_prefix("/api/profiles")?;
    let fields = tail
        .split('/')
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .ok()
                .map(|s| s.into_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    if fields
        .iter()
        .any(|s| s.len() > 512 || s.contains(['/', '\\']) || s.chars().any(char::is_control))
    {
        return None;
    }
    let values = fields.iter().map(String::as_str).collect::<Vec<_>>();
    let (action, args, admin) = match (method, values.as_slice()) {
        ("GET", [""]) => ("registry", json!({}), false),
        ("POST", ["", "text-revisions"]) => ("publish_text", json!({}), true),
        ("POST", ["", "revisions"]) => ("publish_profile", json!({}), true),
        ("GET", ["", "revisions", id, "instructions"]) if !id.is_empty() => {
            ("instructions", json!({"revision_id":id}), false)
        }
        ("POST", ["", mode, "activate", id]) if !mode.is_empty() && !id.is_empty() => {
            ("activate", json!({"mode":mode,"revision_id":id}), true)
        }
        ("DELETE", ["", kind, id]) if !kind.is_empty() && !id.is_empty() => {
            ("delete", json!({"kind":kind,"revision_id":id}), true)
        }
        ("PUT", ["", "chats", id, "binding"]) if !id.is_empty() => (
            "resolve",
            json!({"session_id":id,"apply_revision":true}),
            true,
        ),
        _ => return None,
    };
    Some(Route {
        action,
        arguments: args,
        admin,
    })
}
pub async fn handle(context: Arc<Context>, request: Request, route: Route) -> Response {
    let cors = match auth::authorize(&context, request.headers(), route.admin).await {
        Ok(v) => v,
        Err(v) => return v,
    };
    let mut response = authorized(context, request, route).await;
    response.headers_mut().extend(cors);
    response
}
async fn authorized(context: Arc<Context>, request: Request, mut route: Route) -> Response {
    let mut category = String::new();
    if route.action == "tools" {
        if let Ok(url) = reqwest::Url::parse(&format!("http://localhost{}", request.uri())) {
            category = url
                .query_pairs()
                .find(|(key, _)| key == "category")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
        }
    }
    if matches!(route.action, "publish_text" | "publish_profile" | "resolve") {
        let bytes = match to_bytes(request.into_body(), 2 * 1024 * 1024).await {
            Ok(v) => v,
            Err(_) => {
                return error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Запрос профиля слишком большой.",
                )
            }
        };
        let body = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(v)) => v,
            _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Проверьте поля профиля."),
        };
        let fields = if route.action == "publish_text" {
            vec!["kind", "name", "text", "source_revision_id"]
        } else if route.action == "publish_profile" {
            vec![
                "mode",
                "name",
                "prompt_revision_id",
                "skill_revision_id",
                "additional_skill_revision_ids",
                "tools",
                "model_policy",
                "rag_policy",
                "source_revision_id",
            ]
        } else {
            vec!["mode", "profile_revision_id"]
        };
        let required = if route.action == "publish_text" {
            vec!["kind", "name", "text"]
        } else if route.action == "publish_profile" {
            vec!["mode", "name", "prompt_revision_id", "skill_revision_id"]
        } else {
            vec!["mode", "profile_revision_id"]
        };
        if required
            .iter()
            .any(|key| !body.get(*key).is_some_and(Value::is_string))
            || body.get("name").is_some_and(|v| {
                !v.as_str()
                    .is_some_and(|s| (1..=160).contains(&s.chars().count()))
            })
        {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Проверьте обязательные поля профиля.",
            );
        }
        for field in fields {
            if let Some(value) = body.get(field) {
                route.arguments[field] = value.clone();
            }
        }
        if route.action == "publish_profile" {
            if body
                .get("tools")
                .is_some_and(|v| !v.as_array().is_some_and(|v| v.iter().all(Value::is_string)))
                || body.get("additional_skill_revision_ids").is_some_and(|v| {
                    !v.as_array()
                        .is_some_and(|v| v.len() <= 15 && v.iter().all(Value::is_string))
                })
                || ["model_policy", "rag_policy"]
                    .iter()
                    .any(|key| body.get(*key).is_some_and(|v| !v.is_object()))
            {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Проверьте инструменты и настройки профиля.",
                );
            }
            if !route.arguments["tools"].is_array() {
                route.arguments["tools"] = json!([]);
            }
        }
        if route.action == "resolve" {
            route.arguments["requested_mode"] = route.arguments["mode"].clone();
            route.arguments["requested_revision_id"] =
                route.arguments["profile_revision_id"].clone();
        }
    }
    let Ok(permit) = context.workspace_slots.clone().try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Профили заняты. Повторите действие.",
        );
    };
    let database = context.database.clone();
    let action = route.action;
    let result=tokio::task::spawn_blocking(move || {
        let _permit=permit;
        if action=="tools" {
            return les_core::tool_registry::execute(&json!({"schema":"les.tool-registry.v1","database":database,"action":"registry","category":category}));
        }
        let result=les_core::chat_profiles::execute(&json!({"schema":"les.chat-profiles.v1","database":database,
            "action":if action=="instructions"{"snapshot"}else{action},"arguments":route.arguments}));
        if action=="instructions" && result["status"]=="completed" {
            return match les_core::chat_profiles::instructions(&result["value"]) {
                Ok(text)=>json!({"status":"completed","value":{"revision_id":route.arguments["revision_id"],"text":text}}),
                Err(e)=>json!({"status":"failed","code":e.code,"detail":e.detail}),
            };
        }
        result
    }).await;
    match result {
        Ok(value) if value["status"] == "completed" => {
            json_response(StatusCode::OK, value["value"].clone(), "rust.profiles")
        }
        Ok(value) => {
            let status = if value["code"] == "PROFILE_INVALID" {
                StatusCode::CONFLICT
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            let detail = value["detail"]
                .as_str()
                .unwrap_or("Не удалось прочитать профили. Повторите действие.");
            if detail.starts_with("profile_text_too_long:") {
                return json_response(
                    status,
                    json!({"detail":{"code":"profile_text_too_long","message":detail}}),
                    "rust.profiles",
                );
            }
            error(status, detail)
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось завершить запись профиля. Проверьте список редакций.",
        ),
    }
}
