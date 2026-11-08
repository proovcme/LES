//! Authorization for direct local requests to native routes.
use crate::{error, transport, Context};
use axum::{
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use les_core::auth::{self, Denial, Key};
use serde_json::Value;
use std::time::Duration;

pub const POLICY_PATH: &str = "/api/light/native-auth-policy";
type Decision<T> = Result<T, Response>;

pub enum Policy {
    Local(LocalPolicy),
    Legacy,
}

pub struct LocalPolicy {
    pub trusted_role: Option<String>,
    proxy_header: HeaderName,
    cors_origins: Vec<String>,
    cors_loopback: bool,
}

pub fn unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Не удалось проверить доступ к диалогу. Дождитесь восстановления Леса и повторите действие.",
    )
}

async fn load_policy(context: &Context) -> Result<Policy, ()> {
    let mut response = context
        .client
        .get(format!("{}{POLICY_PATH}", context.upstream))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| ())?;
    if response.status() == StatusCode::NOT_FOUND {
        // Older explicitly retained backend, or a non-default policy requiring the legacy evaluator.
        return Ok(Policy::Legacy);
    }
    if response.status() != StatusCode::OK {
        return Err(());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len() + chunk.len() > 16_384 {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    parse_policy(&value)
}

fn parse_policy(value: &Value) -> Result<Policy, ()> {
    if value["schema"] != "les.native-auth-policy.v1" {
        return Err(());
    }
    if !value["compatible"].as_bool().ok_or(())? {
        return Ok(Policy::Legacy);
    }
    let proxy_header = match value["proxy_header"].as_str().ok_or(())?.parse() {
        Ok(header) => header,
        Err(_) => return Ok(Policy::Legacy),
    };
    let trusted_role = match &value["trusted_role"] {
        Value::Null => None,
        Value::String(role) if role.is_empty() => None,
        Value::String(role) => Some(role.clone()),
        _ => return Err(()),
    };
    let cors_origins = value["cors_origins"]
        .as_array()
        .ok_or(())?
        .iter()
        .map(|origin| origin.as_str().map(str::to_owned).ok_or(()))
        .collect::<Result<_, _>>()?;
    let cors_loopback = value["cors_loopback"].as_bool().ok_or(())?;
    Ok(Policy::Local(LocalPolicy {
        trusted_role,
        proxy_header,
        cors_origins,
        cors_loopback,
    }))
}

/// Explicit startup policy allows a cold native API without consulting Python.
/// A retained custom policy is never silently replaced with permissive defaults.
pub fn from_file(path: &std::path::Path) -> Result<Policy, &'static str> {
    use std::io::Read;
    if !path.is_absolute() {
        return Err("Absolute native authorization policy path required");
    }
    let file = std::fs::File::open(path).map_err(|_| "Native authorization policy unavailable")?;
    let mut bytes = Vec::new();
    file.take(16_385)
        .read_to_end(&mut bytes)
        .map_err(|_| "Native authorization policy unavailable")?;
    if bytes.len() > 16_384 {
        return Err("Native authorization policy too large");
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid native authorization policy")?;
    match parse_policy(&value).map_err(|_| "Invalid native authorization policy")? {
        Policy::Local(policy) => Ok(Policy::Local(policy)),
        Policy::Legacy => Err("Native startup policy requires a compatible local policy"),
    }
}

fn loopback_cors(origin: &str) -> bool {
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let host = if let Some((host, port)) = authority.split_once(':') {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        host
    } else {
        authority
    };
    matches!(host, "127.0.0.1" | "localhost")
}

pub fn cors(policy: &LocalPolicy, headers: &HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    let Some(origin) = headers.get(header::ORIGIN) else {
        return result;
    };
    result.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    let origin_text = origin.to_str().unwrap_or_default();
    let wildcard = policy.cors_origins.iter().any(|value| value == "*");
    if wildcard && !headers.contains_key(header::COOKIE) {
        result.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    } else if wildcard
        || policy.cors_origins.iter().any(|value| value == origin_text)
        || policy.cors_loopback && loopback_cors(origin_text)
    {
        result.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        result.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    result
}

pub fn key_from(headers: &HeaderMap) -> String {
    let key = headers
        .get("x-api-key")
        .map(HeaderValue::as_bytes)
        .filter(|value| !value.is_empty());
    let bearer = || {
        let value = headers.get(header::AUTHORIZATION)?.as_bytes();
        value
            .get(..7)
            .filter(|prefix| prefix.eq_ignore_ascii_case(b"bearer "))?;
        value.get(7..)
    };
    let key: String = key
        .or_else(bearer)
        .unwrap_or_default()
        .iter()
        .copied()
        .map(char::from)
        .collect();
    auth::trim(&key).to_owned()
}

async fn lookup(context: &Context, key: String) -> Decision<Key> {
    let permit = tokio::time::timeout(
        Duration::from_secs(3),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?;
    let path = context.state_root.join("data/les_meta.db");
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        auth::active_key(&path, &key)
    })
    .await
    {
        Ok(Ok(key)) => Ok(key),
        Ok(Err(failure)) => {
            eprintln!("native_auth_lookup_failed: {failure:?}");
            Err(unavailable())
        }
        Err(_) => Err(unavailable()),
    }
}

async fn authorize_local(
    context: &Context,
    policy: &LocalPolicy,
    headers: &HeaderMap,
    admin: bool,
) -> Decision<()> {
    let key = key_from(headers);
    let key = if key.is_empty() {
        None
    } else {
        Some(lookup(context, key).await?)
    };
    auth::authorize(key, policy.trusted_role.as_deref(), admin)
        .map(|_| ())
        .map_err(denial_response)
}

pub fn denial_response(denial: Denial) -> Response {
    let (status, detail) = match denial {
        Denial::Required => (StatusCode::UNAUTHORIZED, "Authentication required"),
        Denial::MissingKey => (StatusCode::UNAUTHORIZED, "Invalid or disabled API key"),
        Denial::ExpiredKey => (StatusCode::UNAUTHORIZED, "API key expired"),
        Denial::AdminRequired => (StatusCode::FORBIDDEN, "Admin role required"),
        Denial::UserRequired => (StatusCode::FORBIDDEN, "User role required"),
    };
    error(status, detail)
}

pub async fn local_policy<'a>(
    context: &'a Context,
    clean: &HeaderMap,
) -> Decision<Option<&'a LocalPolicy>> {
    let policy = context
        .auth_policy
        .get_or_try_init(|| load_policy(context))
        .await
        .map_err(|_| unavailable())?;
    let Policy::Local(policy) = policy else {
        return Ok(None);
    };
    // Uvicorn and the legacy security layer both interpret forwarding headers.
    // Keep their policy for proxy requests during the transition.
    if ["x-forwarded-for", "x-real-ip", "x-forwarded-proto"]
        .iter()
        .any(|name| clean.contains_key(*name))
        || clean.contains_key(&policy.proxy_header)
    {
        return Ok(None);
    }
    Ok(Some(policy))
}

pub async fn authorize(context: &Context, headers: &HeaderMap, admin: bool) -> Decision<HeaderMap> {
    let clean = transport::clean(headers, true);
    let Some(policy) = local_policy(context, &clean).await? else {
        return transport::authorize_legacy(context, headers, admin).await;
    };
    let cors = cors(policy, headers);
    match authorize_local(context, policy, &clean, admin).await {
        Ok(()) => Ok(cors),
        Err(mut response) => {
            response.headers_mut().extend(cors);
            Err(response)
        }
    }
}
