//! Read an explicit model catalogue without loading a model or saving a key.
use super::*;
use axum::body::to_bytes;
use futures_util::{future::join_all, StreamExt};
use std::collections::BTreeSet;

const LOCAL: &str = "/api/model-connections/discover-local";
const MODELS: &str = "/api/model-connections/discover-models";
const ENGINE: &str = "rust.model-discovery";
const LIMIT: usize = 512_000;

pub(super) fn handles(method: &str, path: &str) -> bool {
    method == "POST" && matches!(path, LOCAL | MODELS)
}

struct Input {
    base_url: String,
    locality: String,
    secret_value: Option<String>,
}
impl Input {
    fn parse(data: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(data).ok()?;
        let fields = value.as_object()?;
        if fields
            .keys()
            .any(|k| !matches!(k.as_str(), "base_url" | "locality" | "secret_value"))
        {
            return None;
        }
        let base_url = value["base_url"]
            .as_str()
            .filter(|v| !v.is_empty() && v.chars().count() <= 2048)?;
        let locality = match fields.get("locality") {
            None => "loopback",
            Some(v) => v
                .as_str()
                .filter(|v| matches!(*v, "loopback" | "private_network" | "remote"))?,
        };
        let secret_value = match fields.get("secret_value") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_str()?.to_owned()),
        };
        Some(Self {
            base_url: base_url.into(),
            locality: locality.into(),
            secret_value,
        })
    }
}
fn loopback() -> String {
    "loopback".into()
}
fn unavailable(message: &str) -> Value {
    json!({"status":"unavailable","message":message})
}

async fn list(
    context: &Context,
    input: Input,
    timeout: Duration,
) -> std::result::Result<Value, &'static str> {
    let permit = context
        .workspace_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "DISCOVERY_BUSY")?;
    let endpoint = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::model_runtime::validate(&input.base_url, &input.locality, true)
            .map(|v| (v, input.secret_value))
    })
    .await
    .map_err(|_| "DISCOVERY_BUSY")??;
    let (mut endpoint, secret) = endpoint;
    let port = endpoint.url.port_or_known_default();
    if endpoint.locality == "loopback"
        && (port == Some(context.listen_port) || port == context.upstream_port())
    {
        return Err("DISCOVERY_SELF_TARGET");
    }
    let base = endpoint.url.path().trim_end_matches('/');
    let path = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    endpoint.url.set_path(&path);
    let reply = crate::model_http::get_authorized(
        endpoint.url,
        endpoint.addresses,
        secret
            .filter(|v| !v.is_empty())
            .map(|v| format!("Bearer {v}")),
        timeout,
    )
    .await?;
    if matches!(reply.status.as_u16(), 401 | 403) {
        return Err("DISCOVERY_AUTH");
    }
    if !reply.status.is_success() {
        return Err(if reply.status.is_redirection() {
            "DISCOVERY_REDIRECT"
        } else {
            "DISCOVERY_HTTP"
        });
    }
    let mut body = Vec::new();
    let mut source = reply.source;
    while let Some(piece) = source.next().await {
        let piece = piece?;
        if body.len().saturating_add(piece.len()) > LIMIT {
            return Err("DISCOVERY_SIZE");
        }
        body.extend_from_slice(&piece);
    }
    let payload: Value = serde_json::from_slice(&body).map_err(|_| "DISCOVERY_SHAPE")?;
    let rows = payload["data"].as_array().ok_or("DISCOVERY_SHAPE")?;
    let names = rows
        .iter()
        .filter_map(|row| row["id"].as_str())
        .filter(|v| !v.is_empty() && v.chars().count() <= 300 && !v.chars().any(char::is_control))
        .collect::<BTreeSet<_>>();
    Ok(json!({"status":"ok","truncated":names.len()>1000,
        "models":names.iter().take(1000).collect::<Vec<_>>()}))
}

fn failure(code: &str) -> Value {
    unavailable(match code {
        "DISCOVERY_AUTH"=>"Проверьте API-ключ: сервис не разрешил получить список моделей. Имя модели также можно ввести вручную.",
        "DISCOVERY_SIZE"=>"Список моделей слишком большой. Введите имя вручную.",
        "DISCOVERY_SHAPE"=>"Сервис вернул неподдерживаемый список. Введите имя модели вручную.",
        "UPSTREAM_TIMEOUT"=>"Сервис не ответил вовремя. Проверьте адрес и запущен ли сервер.",
        "DISCOVERY_BUSY"=>"Лес занят. Повторите поиск моделей.",
        "UPSTREAM_UNREACHABLE" | "UPSTREAM_STREAM_INTERRUPTED" | "DISCOVERY_HTTP"=>"Не удалось получить модели. Проверьте адрес и доступность сервиса.",
        _=>"Проверьте адрес сервиса и выбранное расположение. Переадресация не поддерживается.",
    })
}

pub(super) async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match auth::authorize(&context, request.headers(), true).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if !auth::origin_allowed(request.headers(), &cors) {
        return error(StatusCode::FORBIDDEN, "Источник запроса не разрешён.");
    }
    let mut response = if request.uri().path() == LOCAL {
        let engines = [
            ("ollama", "Ollama", 11434, "/v1", Some("ollama")),
            (
                "lm_studio",
                "LM Studio / совместимый сервер",
                1234,
                "/v1",
                Some("lm_studio"),
            ),
            ("local_api", "Локальный API · порт 8080", 8080, "/v1", None),
            (
                "lemonade",
                "Lemonade / совместимый сервер",
                13305,
                "/api/v1",
                Some("lemonade"),
            ),
            (
                "freetoken",
                "FreeToken / совместимый сервер",
                1919,
                "/v1",
                Some("freetoken"),
            ),
        ];
        let found = join_all(engines.map(|(id,name,port,path,extension)| {
            let owner = context.clone();
            async move {
                let base_url = format!("http://127.0.0.1:{port}{path}");
                let timeout = Duration::from_secs(3);
                let result = tokio::time::timeout(timeout, list(&owner, Input {
                    base_url:base_url.clone(), locality:loopback(), secret_value:None,
                }, timeout)).await.ok()?.ok()?;
                Some(json!({"id":id,"name":name,"base_url":base_url,"extension_type":extension,"models":result["models"]}))
            }
        })).await.into_iter().flatten().collect::<Vec<_>>();
        json_response(
            StatusCode::OK,
            json!({"status":"ok","engines":found}),
            ENGINE,
        )
    } else {
        let timeout = Duration::from_secs(8);
        let data = match tokio::time::timeout(timeout, to_bytes(request.into_body(), 8192)).await {
            Ok(Ok(data)) => data,
            _ => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Не удалось прочитать запрос на поиск моделей.",
                )
            }
        };
        let Some(input) = Input::parse(&data) else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Проверьте адрес и расположение сервиса.",
            );
        };
        let value = match tokio::time::timeout(timeout, list(&context, input, timeout)).await {
            Ok(Ok(value)) => value,
            Ok(Err(code)) => failure(code),
            Err(_) => failure("UPSTREAM_TIMEOUT"),
        };
        json_response(StatusCode::OK, value, ENGINE)
    };
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
