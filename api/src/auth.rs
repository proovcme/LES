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

pub type Policy = LocalPolicy;

pub struct LocalPolicy {
    trusted_role: Option<String>,
    proxy_header: HeaderName,
    cors_origins: Vec<String>,
    cors_loopback: bool,
    native: Option<les_core::access_policy::Policy>,
}

pub fn unavailable() -> Response {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Не удалось проверить права доступа. Проверьте настройки доступа и журнал Леса, затем повторите действие.",
    )
}

async fn load_policy(context: &Context) -> Result<Policy, &'static str> {
    let path = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    tokio::task::spawn_blocking(move || {
        let native = les_core::access_policy::load(&path, |key| std::env::var(key).ok())?;
        let proxy_header = native
            .proxy_header
            .parse()
            .map_err(|_| "AUTH_PROXY_HEADER_INVALID")?;
        Ok(LocalPolicy {
            trusted_role: None,
            proxy_header,
            cors_origins: Vec::new(),
            cors_loopback: false,
            native: Some(native),
        })
    })
    .await
    .map_err(|_| "AUTH_SETTINGS_OWNER_FAILED")?
    .map_err(|code: &'static str| {
        eprintln!(
            "{}",
            serde_json::json!({"event":"native_auth_settings_rejected","code":code})
        );
        code
    })
}

fn policy_failure(code: &str) -> Response {
    let detail = match code {
        "AUTH_SETTINGS_EXPANSION_UNSUPPORTED" => "В настройках доступа указана переменная вместо значения. Укажите явные сети и перезапустите Лес.",
        "AUTH_CORS_PATTERN_UNSUPPORTED" => "Не удалось прочитать правило разрешённых источников. Укажите явные адреса в CORS_ALLOWED_ORIGINS или поддерживаемое правило.",
        "AUTH_PROXY_HEADER_INVALID" => "В настройке доверенного прокси некорректное имя заголовка. Исправьте TRUSTED_PROXY_HEADER.",
        "AUTH_SETTINGS_INVALID" => "Повреждены настройки доступа: проверьте кавычки и значения в файле настроек Леса.",
        _ => "Не удалось загрузить настройки доступа. Проверьте доступ к файлу настроек и журнал Леса.",
    };
    crate::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({"detail":detail,"code":code}),
        "rust.auth-policy",
    )
}

fn parse_policy(value: &Value) -> Result<Policy, ()> {
    if value["schema"] != "les.native-auth-policy.v1" {
        return Err(());
    }
    if !value["compatible"].as_bool().ok_or(())? {
        return Err(());
    }
    let proxy_header = match value["proxy_header"].as_str().ok_or(())?.parse() {
        Ok(header) => header,
        Err(_) => return Err(()),
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
    Ok(LocalPolicy {
        trusted_role,
        proxy_header,
        cors_origins,
        cors_loopback,
        native: None,
    })
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
    parse_policy(&value).map_err(|_| "Invalid native authorization policy")
}

impl LocalPolicy {
    pub fn trusted(&self, headers: &HeaderMap) -> Option<&str> {
        if let Some(native) = &self.native {
            let text = |key| {
                headers
                    .get(key)
                    .map(|v| v.to_str().unwrap_or("invalid-header"))
            };
            return native.trusted_role(
                text("x-forwarded-for"),
                text("x-real-ip"),
                headers
                    .get(&self.proxy_header)
                    .map(|v| v.to_str().unwrap_or("invalid-header")),
            );
        }
        // v1 snapshots describe direct loopback only. They cannot confer trust
        // on a forwarded foreign request; an explicit valid key still works.
        if headers.contains_key("x-forwarded-for")
            || headers.contains_key("x-real-ip")
            || headers.contains_key(&self.proxy_header)
        {
            return None;
        }
        self.trusted_role.as_deref()
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
    let wildcard = policy
        .native
        .as_ref()
        .map(|p| p.wildcard_cors())
        .unwrap_or_else(|| policy.cors_origins.iter().any(|value| value == "*"));
    if wildcard && !headers.contains_key(header::COOKIE) {
        result.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    } else if policy
        .native
        .as_ref()
        .is_some_and(|p| p.allows_cors(origin_text))
        || wildcard
        || policy.cors_origins.iter().any(|value| value == origin_text)
        || policy.cors_loopback && loopback_cors(origin_text)
    {
        result.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
        result.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    result
}

/// CORS permission checks carry no credentials and never call a retained API.
pub async fn preflight(context: &Context, headers: &HeaderMap) -> Response {
    let policy = match local_policy(context).await {
        Ok(policy) => policy,
        Err(response) => return response,
    };
    let mut cors = cors(policy, headers);
    let method = headers
        .get(header::ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let methods = "DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT";
    if !methods.split(", ").any(|allowed| allowed == method)
        || !cors.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    {
        return error(
            StatusCode::BAD_REQUEST,
            "Запрос из этого источника или с этим методом не разрешён.",
        );
    }
    if let Some(requested) = headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
        let valid = requested.to_str().ok().is_some_and(|text| {
            text.len() <= 16_384
                && text
                    .split(',')
                    .all(|name| name.trim().parse::<HeaderName>().is_ok())
        });
        if !valid {
            return error(StatusCode::BAD_REQUEST, "Некорректные заголовки запроса.");
        }
        cors.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
    }
    // Credentials require a concrete origin for preflight, even for wildcard.
    if let Some(origin) = headers.get(header::ORIGIN) {
        cors.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    }
    cors.insert(header::VARY, HeaderValue::from_static("Origin"));
    cors.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(methods),
    );
    cors.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    let mut response = crate::json_response(
        StatusCode::OK,
        serde_json::json!({"allowed":true}),
        "rust.auth-policy",
    );
    response.headers_mut().extend(cors);
    response
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
    auth::authorize(key, policy.trusted(headers), admin)
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

pub async fn local_policy<'a>(context: &'a Context) -> Decision<&'a LocalPolicy> {
    let policy = context
        .auth_policy
        .get_or_try_init(|| load_policy(context))
        .await
        .map_err(|code| policy_failure(code))?;
    Ok(policy)
}

pub async fn authorize(context: &Context, headers: &HeaderMap, admin: bool) -> Decision<HeaderMap> {
    let clean = transport::clean(headers, true);
    let policy = local_policy(context).await?;
    let cors = cors(policy, headers);
    match authorize_local(context, policy, &clean, admin).await {
        Ok(()) => Ok(cors),
        Err(mut response) => {
            response.headers_mut().extend(cors);
            Err(response)
        }
    }
}
