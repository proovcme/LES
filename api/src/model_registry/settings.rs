//! Native model settings mutations; the catalog owns CAS and credential writes.
use super::*;
use axum::body::to_bytes;

fn route<'a>(method: &str, path: &'a str) -> Option<(&'static str, &'a str)> {
    let tail = path.strip_prefix("/api/model-connections")?;
    match (method, tail.split('/').collect::<Vec<_>>().as_slice()) {
        ("POST", [""]) => Some(("create", "")),
        ("POST", ["", id, "revisions"]) if !id.is_empty() => Some(("revise", id)),
        ("POST", ["", id, "disable"]) if !id.is_empty() => Some(("disable", id)),
        ("POST", ["", id, "secret"]) if !id.is_empty() => Some(("secret", id)),
        ("POST", ["", id, "test"]) if !id.is_empty() => Some(("test", id)),
        ("PUT", ["", "roles", role @ ("answer" | "embeddings" | "local_fallback")]) => {
            Some(("bind", role))
        }
        _ => None,
    }
}
pub fn handles(method: &str, path: &str) -> bool {
    route(method, path).is_some()
}
fn fail(status: StatusCode, code: &str) -> Response {
    let message = match code {
        "CONNECTION_REVISION_CONFLICT" | "ROLE_BINDING_CONFLICT" => {
            "Настройки уже изменились. Обновите список и повторите."
        }
        "BOUND_CONNECTION_CONFIRMATION_REQUIRED" => {
            "Эта модель назначена на роль. Подтвердите отключение."
        }
        "CONNECTION_SECRET_ENDPOINT_CHANGED" => {
            "Создайте подключение к новому адресу с отдельным ключом."
        }
        "DISPLAY_NAME_IN_USE" => "Подключение с таким названием уже есть.",
        "CAPABILITY_REQUIRED"
        | "CAPABILITY_SNAPSHOT_INVALID"
        | "CAPABILITY_SNAPSHOT_STALE"
        | "CAPABILITY_EVIDENCE_INSUFFICIENT" => {
            "Сначала проверьте возможности этой ревизии модели."
        }
        "CONNECTION_NOT_FOUND" | "CONNECTION_REVISION_NOT_FOUND" => "Подключение не найдено.",
        "SECRET_VALUE_INVALID" => "Введите действующий ключ без переноса строки.",
        "ENDPOINT_INVALID" => "Проверьте адрес сервиса, HTTPS и выбранное расположение.",
        "INVALID_MODEL_SETTINGS" => "Проверьте название, адрес и имя модели.",
        _ => "Не удалось сохранить настройки модели. Проверьте подключение и повторите.",
    };
    json_response(
        status,
        json!({"code":code,"detail":{"code":code,"message":message}}),
        "rust.model-settings",
    )
}
type Rejected = (StatusCode, &'static str);
const INVALID: Rejected = (StatusCode::UNPROCESSABLE_ENTITY, "INVALID_MODEL_SETTINGS");

fn valid_text(value: &Value, max: usize) -> Option<&str> {
    value.as_str().filter(|v| {
        !v.trim().is_empty() && v.chars().count() <= max && !v.chars().any(char::is_control)
    })
}
fn validate_fields(action: &str, value: &Value) -> Result<(), Rejected> {
    let data = value.as_object().ok_or(INVALID)?;
    let allowed: &[&str] = match action {
        "create" => &[
            "display_name",
            "base_url",
            "model_id",
            "locality",
            "requested_context_tokens",
            "extension_type",
            "secret_value",
        ],
        "revise" => &[
            "expected_revision_id",
            "display_name",
            "base_url",
            "model_id",
            "locality",
            "requested_context_tokens",
            "extension_type",
        ],
        "disable" => &["expected_revision_id", "confirm_bound_roles"],
        "secret" => &["expected_revision_id", "secret_value"],
        "bind" => &["connection_revision_id", "expected_binding_revision"],
        _ => return Err(INVALID),
    };
    if data.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(INVALID);
    }
    for (key, max) in [
        ("display_name", 160),
        ("base_url", 2048),
        ("model_id", 300),
        ("locality", 32),
        ("expected_revision_id", 256),
        ("connection_revision_id", 256),
        ("extension_type", 80),
        ("secret_value", 4096),
    ] {
        if data.get(key).is_some_and(|v| {
            !(valid_text(v, max).is_some()
                || v.is_null()
                    && (key == "extension_type" || action == "create" && key == "secret_value"))
        }) {
            return Err(INVALID);
        }
    }
    if ["requested_context_tokens", "expected_binding_revision"]
        .iter()
        .any(|key| !value[*key].is_null() && value[*key].as_i64().is_none_or(|n| n < 1))
        || data
            .get("confirm_bound_roles")
            .is_some_and(|v| !v.is_boolean())
    {
        return Err(INVALID);
    }
    let required: &[&str] = match action {
        "create" => &["display_name", "base_url", "model_id", "locality"],
        "secret" => &["expected_revision_id", "secret_value"],
        "bind" => &["connection_revision_id"],
        _ => &["expected_revision_id"],
    };
    if required.iter().any(|key| !value[*key].is_string()) {
        return Err(INVALID);
    }
    Ok(())
}
fn domain(code: &str, detail: &str) -> Rejected {
    match (code, detail) {
        (_, "CONNECTION_NOT_FOUND") => (StatusCode::NOT_FOUND, "CONNECTION_NOT_FOUND"),
        (_, "CONNECTION_REVISION_NOT_FOUND") => {
            (StatusCode::NOT_FOUND, "CONNECTION_REVISION_NOT_FOUND")
        }
        (_, "CONNECTION_REVISION_CONFLICT") => {
            (StatusCode::CONFLICT, "CONNECTION_REVISION_CONFLICT")
        }
        (_, "ROLE_BINDING_CONFLICT") => (StatusCode::CONFLICT, "ROLE_BINDING_CONFLICT"),
        (_, "DISPLAY_NAME_IN_USE") => (StatusCode::CONFLICT, "DISPLAY_NAME_IN_USE"),
        (_, "BOUND_CONNECTION_CONFIRMATION_REQUIRED") => (
            StatusCode::CONFLICT,
            "BOUND_CONNECTION_CONFIRMATION_REQUIRED",
        ),
        (_, "CONNECTION_SECRET_ENDPOINT_CHANGED") => {
            (StatusCode::CONFLICT, "CONNECTION_SECRET_ENDPOINT_CHANGED")
        }
        (_, "CAPABILITY_REQUIRED") => (StatusCode::UNPROCESSABLE_ENTITY, "CAPABILITY_REQUIRED"),
        (_, "CAPABILITY_SNAPSHOT_INVALID") => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "CAPABILITY_SNAPSHOT_INVALID",
        ),
        (_, "CAPABILITY_SNAPSHOT_STALE") => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "CAPABILITY_SNAPSHOT_STALE",
        ),
        (_, "CAPABILITY_EVIDENCE_INSUFFICIENT") => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "CAPABILITY_EVIDENCE_INSUFFICIENT",
        ),
        (_, "SECRET_VALUE_INVALID") => (StatusCode::UNPROCESSABLE_ENTITY, "SECRET_VALUE_INVALID"),
        ("WORKSPACE_INVALID", _) => INVALID,
        ("MODEL_REGISTRY_ERROR", _) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "MODEL_SETTINGS_REJECTED")
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "MODEL_SETTINGS_UNAVAILABLE",
        ),
    }
}
fn apply(
    context: &Context,
    action: &str,
    id: &str,
    actor: &str,
    mut input: Value,
) -> Result<Value, Rejected> {
    validate_fields(action, &input)?;
    let env = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    let catalog = |action, args| crate::library::catalog(&context.database, action, args);
    let secret = input
        .as_object_mut()
        .unwrap()
        .remove("secret_value")
        .and_then(|v| v.as_str().map(str::to_owned));
    let endpoint = |url: &Value, locality: &Value| -> Result<String, Rejected> {
        let failed = (StatusCode::UNPROCESSABLE_ENTITY, "ENDPOINT_INVALID");
        let local = locality.as_str().ok_or(failed)?;
        let endpoint = crate::model_runtime::validate(url.as_str().ok_or(failed)?, local, true)
            .map_err(|_| failed)?;
        let port = endpoint.url.port_or_known_default();
        if local == "loopback"
            && (port == Some(context.listen_port) || port == context.upstream_port())
        {
            return Err(failed);
        }
        Ok(endpoint.url.as_str().trim_end_matches('/').to_owned())
    };
    if action == "create" {
        input["base_url"] = json!(endpoint(&input["base_url"], &input["locality"])?);
    } else if action == "revise" {
        let current = catalog("model_registry_get", json!({"connection_id":id}));
        if current["status"] != "completed" {
            return Err(domain(
                current["code"].as_str().unwrap_or(""),
                current["detail"].as_str().unwrap_or(""),
            ));
        }
        let expected = input
            .as_object_mut()
            .unwrap()
            .remove("expected_revision_id")
            .unwrap();
        if current["value"]["revision_id"] != expected {
            return Err((StatusCode::CONFLICT, "CONNECTION_REVISION_CONFLICT"));
        }
        if input.get("base_url").is_some() || input.get("locality").is_some() {
            let canonical = endpoint(
                input
                    .get("base_url")
                    .unwrap_or(&current["value"]["base_url"]),
                input
                    .get("locality")
                    .unwrap_or(&current["value"]["locality"]),
            )?;
            if input.get("base_url").is_some() {
                input["base_url"] = json!(canonical);
            }
        }
        input = json!({"expected_revision_id":expected,"changes":input});
    }
    input["actor"] = json!(actor);
    if action == "bind" {
        input["role"] = json!(id);
    } else if action != "create" {
        input["connection_id"] = json!(id);
    }
    let mut value = les_core::catalog::manage(
        &context.database,
        &env,
        action,
        input.as_object().unwrap(),
        secret.as_deref(),
    )
    .map_err(|e| domain(e.code, &e.detail))?;
    if action == "bind" {
        return Ok(value);
    }
    value["enabled"] = json!(i64::from(value["enabled"] == true || value["enabled"] == 1));
    let snapshot = catalog(
        "model_registry_snapshot",
        json!({"connection_revision_id":value["revision_id"]}),
    );
    value["snapshot"] = snapshot["value"].clone();
    let projected = project(json!({"connections":[value]}), &env).map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "MODEL_SETTINGS_UNAVAILABLE",
        )
    })?;
    Ok(projected["connections"][0].clone())
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let (cors, caller) = match auth::authorize_caller(&context, request.headers(), true).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if !auth::origin_allowed(request.headers(), &cors) {
        return fail(StatusCode::FORBIDDEN, "ORIGIN_DENIED");
    }
    let Some((action, id)) = route(request.method().as_str(), request.uri().path()) else {
        return fail(StatusCode::NOT_FOUND, "CONNECTION_NOT_FOUND");
    };
    let Ok(id) = percent_encoding::percent_decode_str(id)
        .decode_utf8()
        .map(|s| s.into_owned())
    else {
        return fail(INVALID.0, INVALID.1);
    };
    if id.len() > 256 || id.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        return fail(INVALID.0, INVALID.1);
    }
    let body =
        tokio::time::timeout(Duration::from_secs(8), to_bytes(request.into_body(), 16384)).await;
    let input = body
        .ok()
        .and_then(Result::ok)
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let mut response = if let Some(input) = input {
        let actor = format!("admin:{}", &les_core::sha256_hex(caller.as_bytes())[..16]);
        if action == "test" {
            match super::probe::run(context, &id, &actor, &input).await {
                Ok(value) => json_response(StatusCode::OK, value, "rust.model-settings"),
                Err(code) => fail(
                    if code == "CONNECTION_REVISION_NOT_FOUND" {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::UNPROCESSABLE_ENTITY
                    },
                    code,
                ),
            }
        } else {
            match context.workspace_slots.clone().try_acquire_owned() {
                Ok(permit) => match tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    apply(&context, action, &id, &actor, input)
                })
                .await
                {
                    Ok(Ok(value)) => json_response(
                        if action == "create" {
                            StatusCode::CREATED
                        } else {
                            StatusCode::OK
                        },
                        value,
                        "rust.model-settings",
                    ),
                    Ok(Err((status, code))) => fail(status, code),
                    _ => fail(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "MODEL_SETTINGS_UNAVAILABLE",
                    ),
                },
                Err(_) => fail(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "MODEL_SETTINGS_UNAVAILABLE",
                ),
            }
        }
    } else {
        fail(INVALID.0, INVALID.1)
    };
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
