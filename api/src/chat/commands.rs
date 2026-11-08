use crate::Context;
use axum::{extract::Request, http::StatusCode, response::Response};
use serde_json::json;
use std::sync::Arc;
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match crate::auth::authorize(&context, request.headers(), false).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut response = if request.method() == "GET" {
        crate::json_response(
            StatusCode::OK,
            json!({"commands":les_core::chat_commands::list()}),
            "rust.chat-commands",
        )
    } else {
        crate::error(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
    };
    response.headers_mut().extend(cors);
    response
}
