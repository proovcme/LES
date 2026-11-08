use super::*;
use axum::{
    body::to_bytes,
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::Response,
};
pub const PRIVATE: &str = "/api/light/internal-context";
fn session(path: &str) -> Option<&str> {
    path.strip_prefix("/api/workspace/memory/context/")?
        .strip_suffix("/summarize")
        .filter(|s| !s.is_empty() && !s.contains('/'))
}
pub fn handles(path: &str) -> bool {
    path == PRIVATE || session(path).is_some()
}
fn failure(code: &str) -> Response {
    let status = if matches!(code, "SUMMARY_CONTEXT_CHANGED" | "WORKSPACE_CONFLICT") {
        StatusCode::CONFLICT
    } else if code == "SUMMARY_SESSION_NOT_FOUND" {
        StatusCode::NOT_FOUND
    } else if code == "SUMMARY_REQUEST_INVALID" || code == "SUMMARY_SESSION_INVALID" {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    crate::json_response(
        status,
        json!({"code":code,"detail":if status==StatusCode::CONFLICT{"Память изменилась. Обновите окно."}else{"Не удалось обновить сводку. История сохранена; проверьте журнал Леса."}}),
        "rust.conversation",
    )
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let private = request.uri().path() == PRIVATE;
    let cors = if private {
        if context
            .local_model
            .as_ref()
            .is_none_or(|b| !b.authorized(request.headers()))
        {
            return crate::error(StatusCode::NOT_FOUND, "Not Found");
        }
        HeaderMap::new()
    } else {
        match crate::auth::authorize(&context, request.headers(), false).await {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
    let mut response = authorized(context, request, private).await;
    response.headers_mut().extend(cors);
    response
}
async fn authorized(context: Arc<Context>, request: Request, private: bool) -> Response {
    if request.method() != "POST" {
        return crate::error(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    }
    let sid = session(request.uri().path()).map(str::to_owned);
    let bytes = match to_bytes(request.into_body(), 8192).await {
        Ok(v) => v,
        Err(_) => return failure("SUMMARY_REQUEST_INVALID"),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(Value::Object(v)) => Value::Object(v),
        _ => return failure("SUMMARY_REQUEST_INVALID"),
    };
    let sid = sid.or_else(|| body["session_id"].as_str().map(str::to_owned));
    let valid_sid = sid
        .as_deref()
        .is_some_and(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control));
    let action = if private {
        body["action"].as_str().unwrap_or("")
    } else {
        "summarize"
    };
    if !valid_sid && action != "foreground_begin" && action != "foreground_end" {
        return failure("SUMMARY_SESSION_INVALID");
    }
    if body.get("force").is_some_and(|v| !v.is_boolean())
        || body
            .get("expected_revision")
            .is_some_and(|v| v.as_i64().is_none_or(|n| n < 0))
    {
        return failure("SUMMARY_REQUEST_INVALID");
    }
    if !private && body["expected_revision"].as_i64().is_none() {
        return failure("SUMMARY_REQUEST_INVALID");
    }
    if !matches!(
        action,
        "summarize" | "enqueue" | "foreground_begin" | "foreground_end" | "status"
    ) {
        return failure("SUMMARY_REQUEST_INVALID");
    }
    if let Err(e) = crate::chat::http::initialize(&context).await {
        return failure(&e.code);
    }
    let result = match action {
        "foreground_begin" => Foreground::new(context.clone())
            .reserve()
            .await
            .map(|key| json!({"lease":key})),
        "foreground_end" => scheduler::end(
            &context,
            body["lease"].as_str().unwrap_or(""),
            sid.as_deref(),
        )
        .await
        .map(|_| Value::Null),
        "enqueue" => enqueue(&context, sid.as_deref().unwrap())
            .await
            .map(|_| Value::Null),
        "status" => job(&context, "status", json!({"session_id":sid})).await,
        _ => {
            let _activity = Foreground::new(context.clone());
            execute(
                context.clone(),
                sid.as_deref().unwrap(),
                !private || body["force"] == true,
                body["expected_revision"].as_i64(),
                None,
            )
            .await
        }
    };
    match result {
        Ok(value) => crate::json_response(StatusCode::OK, value, "rust.conversation"),
        Err(code) => failure(&code),
    }
}
