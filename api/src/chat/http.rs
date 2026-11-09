//! One native HTTP turn behind internal and explicitly selected public ingress.
use super::{
    error::Failure,
    journal::{Journal, Service},
    runner, Context, Result, Sink,
};
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{
    future::{select, Either},
    stream,
};
use serde_json::{json, Value};
use std::{convert::Infallible, sync::Arc, time::Duration};
pub const PATH: &str = "/api/light/internal-chat";
pub const STREAM: &str = "/api/light/internal-chat/stream";
const PUBLIC: &str = "/api/chat";
const PUBLIC_STREAM: &str = "/api/chat/stream";
pub fn handles(path: &str) -> bool {
    matches!(path, PATH | STREAM)
}
pub fn public_path(path: &str) -> bool {
    matches!(path, PUBLIC | PUBLIC_STREAM)
}
fn failed(error: Failure) -> Response {
    crate::json_response(error.status, error.payload(), "rust.chat")
}
pub(crate) async fn initialize(context: &Arc<Context>) -> Result<()> {
    let owner = context.clone();
    context
        .chat
        .get_or_try_init(|| async move {
            tokio::task::spawn_blocking(move || Service::new(&owner))
                .await
                .map_err(|_| Failure::new("CHAT_STORAGE_UNAVAILABLE"))?
        })
        .await?;
    crate::conversation::start(context);
    Ok(())
}
async fn receipt(
    context: Arc<Context>,
    request: Value,
    key: Option<String>,
    caller: Option<String>,
) -> Result<Value> {
    initialize(&context).await?;
    let db = context.database.clone();
    tokio::task::spawn_blocking(move || {
        les_core::chat_request::journal::begin_for_caller(
            &db,
            &request,
            key.as_deref(),
            caller.as_deref(),
        )
    })
    .await
    .map_err(|_| "CHAT_STORAGE_UNAVAILABLE")?
    .map_err(Into::into)
}
async fn execute(
    context: Arc<Context>,
    mut request: Value,
    saved: Value,
    sink: Sink,
) -> Result<Value> {
    if saved["created"] != true {
        if saved["status"] == "completed" {
            return Ok(saved["response"].clone());
        }
        return Err("CHAT_REQUEST_NOT_REPLAYABLE".into());
    }
    let mut journal = Journal::new(context.database.clone(), &saved)?;
    let _activity = crate::conversation::Foreground::new(context.clone());
    let sender = sink.sender.clone();
    let result = {
        let run = Box::pin(tokio::time::timeout(
            Duration::from_secs(660),
            runner::run(context, &mut request, &mut journal, &sink),
        ));
        match select(run, Box::pin(sender.closed())).await {
            Either::Left((Ok(value), _)) => value,
            Either::Left((Err(_), _)) => Err("CHAT_DEADLINE_EXCEEDED".into()),
            Either::Right(_) => Err("CHAT_CANCELLED".into()),
        }
    };
    let result = match result {
        Ok(value) => {
            let value = super::view::decorate(value);
            journal.finish(&request, &value).await.map(|_| value)
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            eprintln!(
                "{}",
                json!({"event":"chat_request_failed","history_id":journal.history,"code":error.code})
            );
            if let Err(storage) = journal.fail(&error.payload()).await {
                eprintln!(
                    "{}",
                    json!({"event":"chat_failure_recording_failed","history_id":journal.history,"cause":error.code,"code":storage.code})
                );
            }
            Err(error)
        }
    }
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != "POST"
        || context
            .local_model
            .as_ref()
            .is_none_or(|b| !b.authorized(request.headers()))
    {
        return crate::error(StatusCode::NOT_FOUND, "Not Found");
    }
    handle_turn(context, request, None).await
}

pub async fn handle_public(context: Arc<Context>, request: Request) -> Response {
    let (cors, caller) =
        match crate::auth::authorize_caller(&context, request.headers(), false).await {
            Ok(value) => value,
            Err(response) => return response,
        };
    let headers = request.headers();
    let allowed_origin = headers.get(header::ORIGIN).is_none_or(|origin| {
        cors.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            || headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|host| origin.as_bytes() == format!("http://{host}").as_bytes())
    });
    let mut response = if !allowed_origin {
        crate::error(
            StatusCode::FORBIDDEN,
            "Источник запроса к чату не разрешён.",
        )
    } else if request.method() != Method::POST {
        let mut response =
            crate::error(StatusCode::METHOD_NOT_ALLOWED, "Для чата используйте POST.");
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        response
    } else if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        crate::error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Отправьте запрос чата в формате JSON.",
        )
    } else {
        handle_turn(context, request, Some(caller)).await
    };
    response.headers_mut().extend(cors);
    response
}

async fn handle_turn(context: Arc<Context>, request: Request, caller: Option<String>) -> Response {
    let streaming = matches!(request.uri().path(), STREAM | PUBLIC_STREAM);
    if request.headers().get_all("idempotency-key").iter().count() > 1 {
        return failed("CHAT_REQUEST_KEY_INVALID".into());
    }
    let key = match request.headers().get("idempotency-key").map(|v| v.to_str()) {
        Some(Err(_)) => return failed("CHAT_REQUEST_KEY_INVALID".into()),
        Some(Ok(v)) => Some(v.to_owned()),
        None => None,
    };
    let bytes = match to_bytes(request.into_body(), 512 * 1024).await {
        Ok(v) => v,
        Err(_) => return failed("CHAT_TEXT_LIMIT".into()),
    };
    let input: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return failed("CHAT_REQUEST_INVALID".into()),
    };
    let request = match les_core::chat_request::validate(&input) {
        Ok(v) => v,
        Err(e) => return failed(e.into()),
    };
    if let Err(error) = super::prepare::admit(&request) {
        return failed(error);
    }
    let saved = match receipt(context.clone(), request.clone(), key, caller).await {
        Ok(v) => v,
        Err(e) => return failed(e),
    };
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Value>(32);
    let sink = Sink { sender };
    if !streaming {
        // Poll execution and drain progress together. JSON uses exactly the same
        // backpressure, persistence and provider path as streaming.
        let run = Box::pin(execute(context, request, saved, sink));
        let drain = Box::pin(async move { while receiver.recv().await.is_some() {} });
        let value = match select(run, drain).await {
            Either::Left((value, _)) => value,
            Either::Right((_, run)) => run.await,
        };
        return match value {
            Ok(v) => crate::json_response(StatusCode::OK, v, "rust.chat"),
            Err(e) => failed(e),
        };
    }
    tokio::spawn(async move {
        match execute(context, request, saved, sink.clone()).await {
            Ok(value) => {
                let _ = sink.emit("final", value).await;
            }
            Err(error) => {
                let _=sink.emit("error",json!({"status":error.status.as_u16(),"code":error.code,"detail":error.payload()["detail"],"message":error.payload()["message"]})).await;
            }
        }
    });
    let body = Body::from_stream(stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| {
            let frame = format!(
                "event: {}\ndata: {}\n\n",
                item["event"].as_str().unwrap_or("error"),
                item["data"]
            );
            (Ok::<_, Infallible>(frame), receiver)
        })
    }));
    let mut response = (
        [
            ("content-type", "text/event-stream; charset=utf-8"),
            ("cache-control", "no-cache"),
            ("x-accel-buffering", "no"),
            ("x-les-handler", "rust.chat"),
        ],
        body,
    )
        .into_response();
    response.headers_mut().remove("content-length");
    response
}
