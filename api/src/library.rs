use crate::{auth, error, json_response, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::{sync::Arc, time::Duration};

const FOLDER_WATCHES: &str = "/api/light/folder-watches";

pub fn handles(method: &str, path: &str) -> bool {
    matches!(
        (method, path),
        (
            "GET",
            "/api/light/library" | "/api/light/forest" | "/api/light/intake" | FOLDER_WATCHES
        ) | ("POST", "/api/rag/datasets" | FOLDER_WATCHES)
    )
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let write = request.method() == "POST";
    let cors = match auth::authorize(
        &context,
        request.headers(),
        write || request.uri().path() == FOLDER_WATCHES,
    )
    .await
    {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let mut response = dispatch(context, request, write).await;
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert("x-les-handler", "rust.library".parse().unwrap());
    response
}

async fn dispatch(context: Arc<Context>, request: Request, write: bool) -> Response {
    let uri = request.uri();
    let folder_watches = uri.path() == FOLDER_WATCHES;
    let mut args: Map<String, Value> = reqwest::Url::parse(&format!("http://localhost{uri}"))
        .map(|url| {
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned().into()))
                .collect()
        })
        .unwrap_or_default();
    let action = if write {
        let bytes = match to_bytes(request.into_body(), 16_384).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Название датасета слишком длинное.",
                )
            }
        };
        if !bytes.is_empty() {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Null) => {}
                Ok(Value::Object(body)) => {
                    if folder_watches {
                        args = body;
                    } else if let Some(value) = body.get("name") {
                        if !value.is_string() {
                            return error(
                                StatusCode::UNPROCESSABLE_ENTITY,
                                "Название должно быть строкой.",
                            );
                        }
                        args.insert("name".into(), value.clone());
                    }
                }
                _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Ожидается объект JSON."),
            }
        }
        if folder_watches {
            "intake_watch_set"
        } else {
            args.entry("name").or_insert_with(|| "".into());
            "dataset_create"
        }
    } else if folder_watches {
        if let Some(offset) = args.get_mut("offset") {
            let Some(value) = offset.as_str().and_then(|value| value.parse::<i64>().ok()) else {
                return error(StatusCode::UNPROCESSABLE_ENTITY, "Неверный номер страницы.");
            };
            *offset = value.into();
        }
        "intake_watch_list"
    } else if uri.path() == "/api/light/intake" {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "Укажите ID импорта.");
        };
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "Неверный ID импорта.");
        }
        "job_get"
    } else if uri.path() == "/api/light/forest" {
        "library_forest"
    } else if args.contains_key("dataset_id") {
        for key in ["limit", "offset"] {
            if let Some(value) = args.get_mut(key) {
                let Some(number) = value.as_str().and_then(|s| s.parse::<i64>().ok()) else {
                    return error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "Неверные параметры страницы.",
                    );
                };
                *value = number.into();
            }
        }
        "library_documents"
    } else {
        "library_datasets"
    };
    let permit = match tokio::time::timeout(
        Duration::from_secs(10),
        context.workspace_slots.clone().acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Каталог занят. Повторите действие.",
            )
        }
    };
    let database = context.database.clone();
    let job_database = context.state_root.join("data/les_meta.db");
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if action == "job_get" {
            intake(&job_database, &database, args)
        } else {
            catalog(&database, action, Value::Object(args))
        }
    })
    .await;
    match result {
        Ok(result) if result["status"] == "completed" && result["value"].is_object() => {
            if action == "intake_watch_set" {
                crate::intake::wake(&context);
            }
            json_response(StatusCode::OK, result["value"].clone(), "rust.library")
        }
        Ok(result) if result["code"] == "WORKSPACE_NOT_FOUND" => error(
            StatusCode::NOT_FOUND,
            result["detail"]
                .as_str()
                .unwrap_or("Набор документов не найден."),
        ),
        Ok(result) if result["code"] == "WORKSPACE_INVALID" => error(
            if write {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            },
            result["detail"]
                .as_str()
                .unwrap_or("Проверьте поля запроса."),
        ),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Каталог документов недоступен. Повторите действие.",
        ),
    }
}

pub(crate) fn catalog(database: &Path, action: &str, args: Value) -> Value {
    les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,
        "action":action,"arguments":args}))
}

fn intake(jobs: &Path, database: &Path, args: Map<String, Value>) -> Value {
    let native = catalog(database, "intake_get", Value::Object(args.clone()));
    if native["value"].is_object() {
        return native;
    }
    let result = catalog(jobs, "job_get", Value::Object(args));
    if result["status"] != "completed" {
        return result;
    }
    let job = &result["value"];
    if job["type"] != "dataset_intake" || !job["dataset_id"].is_string() {
        return json!({"code":"WORKSPACE_NOT_FOUND","detail":"Импорт не найден."});
    }
    let dataset = catalog(
        database,
        "library_dataset",
        json!({"dataset_id":job["dataset_id"]}),
    );
    if dataset["value"]["dataset_id"] != job["dataset_id"] {
        return json!({"code":"WORKSPACE_NOT_FOUND","detail":"Импорт не найден."});
    }
    let fields = [
        "id",
        "dataset_id",
        "status",
        "total",
        "processed",
        "errors",
        "started_at",
        "updated_at",
        "finished_at",
    ];
    let row: Map<String, Value> = fields
        .into_iter()
        .map(|k| (k.into(), job[k].clone()))
        .collect();
    json!({"status":"completed","value":row})
}
