use crate::{error, json_response, transport, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match transport::authorize(&context, request.headers()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut response = authorized(context, request).await;
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert("x-les-handler", "rust.agent-state".parse().unwrap());
    response
}
async fn authorized(context: Arc<Context>, request: Request) -> Response {
    let write = request.method() == "POST";
    let mut args = if write {
        let bytes = match to_bytes(request.into_body(), 2_097_152).await {
            Ok(v) => v,
            Err(_) => {
                return error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Текст слишком велик для одного файла.",
                )
            }
        };
        match serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.as_object().cloned())
        {
            Some(v) => v,
            None => return error(StatusCode::UNPROCESSABLE_ENTITY, "Проверьте поля запроса."),
        }
    } else {
        let url = match reqwest::Url::parse(&format!("http://localhost{}", request.uri())) {
            Ok(v) => v,
            Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "Неверный запрос."),
        };
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), Value::String(v.into_owned())))
            .collect::<Map<_, _>>()
    };
    let action = args
        .remove("action")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| if write { "save" } else { "bundle" }.into());
    if (write
        && !matches!(
            action.as_str(),
            "save" | "edit" | "delete" | "restore" | "policy_save"
        ))
        || (!write
            && !matches!(
                action.as_str(),
                "read" | "bundle" | "list" | "history" | "version" | "policy" | "export"
            ))
    {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Недоступное действие.");
    }
    let project = match args.get("project_id") {
        None => Some(0),
        Some(Value::String(v)) if !write => v.parse::<i64>().ok(),
        Some(v) => v.as_i64(),
    };
    let Some(project) = project.filter(|v| *v >= 0) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Выберите проект или общую память.",
        );
    };
    args.insert("project_id".into(), project.into());
    if !write {
        if let Some(value) = args.get("include_deleted") {
            let parsed = match value.as_str() {
                Some("true") => true,
                Some("false") => false,
                _ => {
                    return error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "include_deleted: true или false.",
                    )
                }
            };
            args.insert("include_deleted".into(), parsed.into());
        }
        if let Some(value) = args.get("offset") {
            let Some(offset) = value.as_str().and_then(|s| s.parse::<u64>().ok()) else {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Неверная страница истории.",
                );
            };
            args.insert("offset".into(), offset.into());
        }
    }
    args.insert("actor".into(), "user".into());
    args.entry("area").or_insert_with(|| "memory".into());
    let database = context.database.clone();
    let root = context.state_root.clone();
    let permit = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(p)) => p,
        _ => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Рабочие файлы заняты. Повторите действие.",
            )
        }
    };
    let result=tokio::task::spawn_blocking(move||{let _permit=permit;les_core::agent_files::execute(&json!({"schema":"les.agent-files.v1","state_root":root,"database":database,"action":action,"arguments":args}))}).await;
    match result {
        Ok(result) if result["status"] == "completed" => {
            json_response(StatusCode::OK, result["value"].clone(), "rust.agent-state")
        }
        Ok(result) => {
            let status = match result["code"].as_str() {
                Some("AGENT_CONFLICT") => StatusCode::CONFLICT,
                Some("AGENT_NOT_FOUND") => StatusCode::NOT_FOUND,
                Some("AGENT_DISABLED") => StatusCode::FORBIDDEN,
                Some(
                    "AGENT_INVALID" | "AGENT_ENCODING" | "AGENT_PATH_UNSAFE" | "AGENT_TOO_LARGE",
                ) => StatusCode::UNPROCESSABLE_ENTITY,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            error(
                status,
                result["detail"]
                    .as_str()
                    .unwrap_or("Не удалось прочитать или сохранить рабочие файлы."),
            )
        }
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Запись прервана. Откройте файл заново, чтобы проверить сохранённую версию.",
        ),
    }
}
