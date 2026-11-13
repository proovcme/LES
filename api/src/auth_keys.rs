//! HTTP adapter for the native key registry.
use crate::{auth, error, json_response, transport, Context};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    response::Response,
};
use les_core::auth_keys::{self, Failure, Operation};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

pub enum Route {
    List,
    Create,
    Toggle,
    ResetDevice,
    Delete,
    DeletePath(String),
    Verify,
}

pub fn route(method: &Method, path: &str) -> Option<Route> {
    match (method.as_str(), path) {
        ("GET", "/api/auth/keys") => Some(Route::List),
        ("POST", "/api/auth/keys") => Some(Route::Create),
        ("POST", "/api/auth/keys/toggle") => Some(Route::Toggle),
        ("POST", "/api/auth/keys/reset-device") => Some(Route::ResetDevice),
        ("POST", "/api/auth/keys/delete") => Some(Route::Delete),
        ("POST", "/api/auth/verify") => Some(Route::Verify),
        ("DELETE", path) => {
            let key = percent_encoding::percent_decode_str(path.strip_prefix("/api/auth/keys/")?)
                .decode_utf8()
                .ok()?;
            (!key.is_empty() && !key.contains('/')).then(|| Route::DeletePath(key.into_owned()))
        }
        _ => None,
    }
}

fn invalid(detail: &str) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, detail)
}

fn string(value: &Value, field: &str, default: Option<&str>) -> Result<String, Response> {
    match value.get(field) {
        Some(value) => value.as_str(),
        None => default,
    }
    .map(str::to_owned)
    .ok_or_else(|| invalid("Укажите строковое значение поля ключа"))
}

fn integer(value: &Value, field: &str, default: i64) -> Result<i64, Response> {
    let Some(value) = value.get(field) else {
        return Ok(default);
    };
    let integer = value
        .as_i64()
        .or_else(|| value.as_bool().map(i64::from))
        .or_else(|| value.as_str()?.trim().parse().ok());
    integer
        .or_else(|| {
            let number = value
                .as_f64()
                .or_else(|| value.as_str()?.trim().parse().ok())?;
            (number.fract() == 0.0 && number >= i64::MIN as f64 && number < -(i64::MIN as f64))
                .then_some(number as i64)
        })
        .ok_or_else(|| invalid("Укажите целое число"))
}

async fn operation(request: Request, route: Route) -> Result<Operation, Response> {
    match route {
        Route::List => return Ok(Operation::List),
        Route::DeletePath(key) => return Ok(Operation::Delete(key)),
        _ => {}
    }
    let mut media_allowed = true;
    if let Some(content_type) = request.headers().get(header::CONTENT_TYPE) {
        let json = content_type
            .to_str()
            .ok()
            .and_then(|value| value.split(';').next())
            .and_then(|media| media.trim().split_once('/'))
            .is_some_and(|(kind, subtype)| {
                kind.eq_ignore_ascii_case("application")
                    && (subtype.eq_ignore_ascii_case("json")
                        || subtype.to_ascii_lowercase().ends_with("+json"))
            });
        media_allowed = json;
    }
    let bytes = to_bytes(request.into_body(), 1_048_576)
        .await
        .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE, "Запрос слишком велик"))?;
    // Drain the bounded body before returning a media-type error. Leaving an
    // HTTP/1 upload unread can reset a pooled connection on Windows.
    if !media_allowed {
        return Err(invalid("Ожидается Content-Type: application/json"));
    }
    let body: Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("Ожидается JSON-объект"))?;
    if !body.is_object() {
        return Err(invalid("Ожидается JSON-объект"));
    }
    if matches!(route, Route::Verify) {
        return Ok(Operation::Verify {
            key: string(&body, "key", None)?,
            fingerprint: string(&body, "fingerprint", Some(""))?,
        });
    }
    let key = string(&body, "key_value", None)?;
    Ok(match route {
        Route::Create => Operation::Create {
            key,
            holder: string(&body, "holder_name", Some(""))?,
            role: string(&body, "role", Some("user"))?,
            expires_days: integer(&body, "expires_days", 0)?,
        },
        Route::Toggle => Operation::Toggle {
            key,
            active: integer(&body, "is_active", 1)?,
        },
        Route::ResetDevice => {
            integer(&body, "is_active", 1)?;
            Operation::ResetDevice(key)
        }
        Route::Delete => Operation::Delete(key),
        _ => unreachable!("route handled above"),
    })
}

fn failure(error_code: Failure) -> Response {
    let (status, detail) = match error_code {
        Failure::Access(denial) => return auth::denial_response(denial),
        Failure::Storage => return auth::unavailable(),
        Failure::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
        Failure::Conflict => (StatusCode::CONFLICT, "Ключ уже существует"),
        Failure::RootRequired => (
            StatusCode::FORBIDDEN,
            "Admin/root keys can be created only from trusted network or les-admin key",
        ),
        Failure::TrustedAdminRequired => (
            StatusCode::FORBIDDEN,
            "les-admin key can be changed only from trusted network",
        ),
        Failure::InvalidKey => (StatusCode::UNAUTHORIZED, "Неверный ключ или ключ отключён"),
        Failure::ExpiredKey => (StatusCode::UNAUTHORIZED, "Ключ истёк"),
        Failure::WrongDevice => (StatusCode::FORBIDDEN, "Ключ привязан к другому устройству"),
    };
    error(status, detail)
}

async fn run(
    context: &Context,
    request: Request,
    route: Route,
    credential: String,
    trusted: Option<String>,
) -> Response {
    let operation = match operation(request, route).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let permit = match tokio::time::timeout(
        Duration::from_secs(3),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => return auth::unavailable(),
    };
    let path = context.state_root.join("data/les_meta.db");
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        auth_keys::execute(&path, &credential, trusted.as_deref(), operation)
    })
    .await
    {
        Ok(Ok(value)) => json_response(StatusCode::OK, value, "rust.auth-keys"),
        Ok(Err(error)) => failure(error),
        Err(_) => auth::unavailable(),
    }
}

pub async fn handle(context: Arc<Context>, request: Request, route: Route) -> Response {
    let clean = transport::clean(request.headers(), true);
    let policy = match auth::local_policy(&context).await {
        Ok(policy) => policy,
        Err(response) => return response,
    };
    let cors = auth::cors(policy, request.headers());
    let mut response = run(
        &context,
        request,
        route,
        auth::key_from(&clean),
        policy.trusted(&clean).map(str::to_owned),
    )
    .await;
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert("x-les-handler", HeaderValue::from_static("rust.auth-keys"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
