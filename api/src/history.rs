//! History is read without proxying the request or importing the chat coordinator.
use crate::{auth, error, json_response, Context};
use axum::{
    extract::Request,
    http::{header, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

pub fn handles(method: &str, path: &str) -> bool {
    method == "GET"
        && matches!(
            path,
            "/api/chat/history" | "/api/chat/sessions" | "/api/chat/learning"
        )
}
fn arguments(request: &Request) -> Result<Value, Response> {
    let url = reqwest::Url::parse(&format!("http://localhost{}", request.uri())).map_err(|_| {
        error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Некорректный запрос истории.",
        )
    })?;
    let query = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    let learning = request.uri().path().ends_with("/learning");
    let sessions = request.uri().path().ends_with("/sessions");
    let default = if learning {
        100
    } else if sessions {
        50
    } else {
        40
    };
    let limit = match query.get("limit") {
        None => default,
        Some(value) => value.parse::<i64>().map_err(|_| {
            error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Лимит истории должен быть целым числом.",
            )
        })?,
    };
    let confirmed = match query
        .get("confirmed_only")
        .filter(|_| learning)
        .map(|v| v.to_lowercase())
        .as_deref()
    {
        None | Some("false" | "0" | "off" | "no" | "f" | "n") => false,
        Some("true" | "1" | "on" | "yes" | "t" | "y") => true,
        _ => {
            return Err(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Некорректный фильтр обратной связи.",
            ))
        }
    };
    Ok(
        json!({"limit":limit,"session_id":query.get("session_id").map(|v|v.as_ref()),"confirmed_only":confirmed}),
    )
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match auth::authorize(&context, request.headers(), false).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let args = match arguments(&request) {
        Ok(v) => v,
        Err(mut r) => {
            r.headers_mut().extend(cors);
            return r;
        }
    };
    let action = match request.uri().path() {
        "/api/chat/sessions" => "chat_history_sessions",
        "/api/chat/learning" => "chat_history_http_learning",
        _ => "chat_history_http_messages",
    };
    let permit = match tokio::time::timeout(
        Duration::from_secs(3),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(v)) => v,
        _ => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "История временно занята. Повторите загрузку.",
            )
        }
    };
    let database = context.database.clone();
    let result=tokio::task::spawn_blocking(move||{
        let _permit=permit;
        les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,"action":action,"arguments":args}))
    }).await;
    let mut response = match result {
        Ok(value) if value["status"] == "completed" => {
            json_response(StatusCode::OK, value["value"].clone(), "rust.chat-history")
        }
        Ok(value) => {
            eprintln!(
                "{}",
                json!({"event":"history_read_failed","operation":action,"code":value["code"],"sqlite_extended":value["sqlite_extended"]})
            );
            error(StatusCode::SERVICE_UNAVAILABLE,"Не удалось прочитать историю. Переписка не удалена; повторите загрузку или откройте диагностику.")
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось прочитать историю. Переписка не удалена; повторите загрузку.",
        ),
    };
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
