//! Status for the desktop and terminal when the Rust API owns the application.
use crate::{auth, chat, error, index_batch::configuration, json_response, library, Context};
use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use std::sync::Arc;

const ENGINE: &str = "rust.status";

pub async fn runtime(context: Arc<Context>, headers: &HeaderMap) -> Response {
    let cors = match auth::authorize(&context, headers, false).await {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let Ok(admission) =
        tokio::task::spawn_blocking(move || chat::prepare::admission(&context)).await
    else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Состояние чата недоступно.",
        );
    };
    let mut response = json_response(
        StatusCode::OK,
        json!({"status":"ready","engine":"rust","chat_admission":admission}),
        ENGINE,
    );
    response.headers_mut().extend(cors);
    response
}

pub async fn jobs(context: Arc<Context>, request: Request) -> Response {
    let cors = match auth::authorize(&context, request.headers(), false).await {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let limit = request
        .uri()
        .query()
        .and_then(|query| url_limit(query))
        .unwrap_or(30)
        .clamp(1, 100);
    let Ok(result) = tokio::task::spawn_blocking(move || {
        library::catalog(&context.database, "intake_list", json!({"limit":limit}))
    })
    .await
    else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Очередь обработки недоступна.",
        );
    };
    let Some(rows) = result["value"].as_object() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Очередь обработки недоступна.",
        );
    };
    let mut jobs: Vec<_> = rows.values().collect();
    jobs.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    let mut response = json_response(StatusCode::OK, json!({"jobs":jobs}), ENGINE);
    response.headers_mut().extend(cors);
    response
}

fn url_limit(query: &str) -> Option<i64> {
    reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .ok()?
        .query_pairs()
        .find(|(key, _)| key == "limit")?
        .1
        .parse()
        .ok()
}

pub async fn health(context: &Context) -> Response {
    let result = tokio::time::timeout(std::time::Duration::from_secs(4), index(context)).await;
    let rag = match result {
        Ok(Ok(rag)) => rag,
        _ => json!({"status":"degraded","dense_available":false,"qdrant":{"ok":false}}),
    };
    let status = if rag["status"] == "degraded" {
        "degraded"
    } else {
        "ok"
    };
    json_response(StatusCode::OK, json!({"status":status,"rag":rag}), ENGINE)
}

async fn index(context: &Context) -> Result<Value, &'static str> {
    let settings = configuration::Settings::load(&context.state_root)?;
    let (collection, contract) = configuration::identity(context, &settings)?;
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_SERVICE_UNAVAILABLE")?;
    let info = store
        .setup_call(context, &collection, reqwest::Method::GET, "", Value::Null)
        .await?;
    let Some(_) = info else {
        return Ok(
            json!({"status":"empty","dense_available":false,"qdrant":{"ok":true,"points":0}}),
        );
    };
    let points = store
        .setup_call(
            context,
            &collection,
            reqwest::Method::POST,
            "points/count",
            json!({"exact":true}),
        )
        .await?
        .and_then(|value| value["count"].as_u64())
        .ok_or("INDEX_POINT_COUNT_UNAVAILABLE")?;
    let dense = points > 0
        && store.recovery_error().is_none()
        && les_core::index_bootstrap::read(&contract)?
            .is_some_and(|value| value["collection"] == collection);
    let status = if points == 0 {
        "not_indexed"
    } else if dense {
        "ready"
    } else {
        "degraded"
    };
    Ok(json!({"status":status,"dense_available":dense,"qdrant":{"ok":true,"points":points}}))
}
