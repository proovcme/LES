//! Complete native navigation reader: durable queue -> exact model -> checked JSON -> revision-safe result.
use crate::{auth, error, json_response, library::catalog, model_runtime, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{fs::File, path::Path, sync::Arc, time::Duration};
use tokio::sync::Notify;

pub const PRIVATE: &str = "/api/light/internal-dataset-reader";
pub struct Service {
    wake: Notify,
}
impl Service {
    pub fn new() -> Self {
        Self {
            wake: Notify::new(),
        }
    }
}
pub fn route(path: &str) -> Option<&str> {
    path.strip_prefix("/api/notebooks/")?
        .strip_suffix("/memory/read")
        .or_else(|| {
            path.strip_prefix("/api/notebooks/")?
                .strip_suffix("/memory/reader")
        })
        .filter(|s| !s.is_empty() && !s.contains('/'))
}
async fn call(context: &Context, action: &'static str, args: Value) -> Result<Value, String> {
    let db = context.database.clone();
    tokio::task::spawn_blocking(move || {
        let value = catalog(&db, action, args);
        if value["status"] != "completed" {
            return Err(value["code"]
                .as_str()
                .unwrap_or("READER_STORAGE_UNAVAILABLE")
                .to_owned());
        }
        Ok(value["value"].clone())
    })
    .await
    .map_err(|_| "READER_STORAGE_UNAVAILABLE".to_owned())?
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let private = request.uri().path() == PRIVATE;
    let dataset = route(request.uri().path()).map(str::to_owned);
    let view = request.method() == "GET" && request.uri().path().ends_with("/memory/reader");
    if request.method() != "POST" && !view {
        return error(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    }
    let cors = if private {
        if context
            .index_store
            .as_ref()
            .is_none_or(|s| !s.authorized_internal(request.headers()))
        {
            return error(StatusCode::NOT_FOUND, "Not Found");
        }
        axum::http::HeaderMap::new()
    } else {
        match auth::authorize(&context, request.headers(), !view).await {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
    let bytes = match to_bytes(request.into_body(), 8192).await {
        Ok(v) => v,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "READER_REQUEST_INVALID"),
    };
    let body = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(v)) => Value::Object(v),
            _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "READER_REQUEST_INVALID"),
        }
    };
    let dataset = dataset.or_else(|| body["dataset_id"].as_str().map(str::to_owned));
    let Some(dataset) =
        dataset.filter(|s| !s.is_empty() && s.len() <= 160 && !s.contains(['/', '\\', ':', '\0']))
    else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "READER_REQUEST_INVALID");
    };
    for key in ["background", "force", "allow_remote"] {
        if body.get(key).is_some_and(|v| !v.is_boolean()) {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "READER_REQUEST_INVALID");
        }
    }
    let attempts = body
        .get("max_attempts")
        .map_or(Some(2), Value::as_u64)
        .filter(|n| (1..=3).contains(n));
    let Some(attempts) = attempts else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "READER_REQUEST_INVALID");
    };
    if view {
        let mut response =
            match call(&context, "reader_memory", json!({"dataset_id":dataset})).await {
                Ok(value) => json_response(StatusCode::OK, value, "rust.dataset-reader"),
                Err(code) => error(StatusCode::NOT_FOUND, &code),
            };
        response.headers_mut().extend(cors);
        return response;
    }
    let Some(service) = &context.reader else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Обработка карт документов отключена в этом экземпляре Леса.",
        );
    };
    let outcome=call(&context,"reader_enqueue",json!({"dataset_id":dataset,"force":body["force"].as_bool().unwrap_or(false),"allow_remote":body["allow_remote"].as_bool().unwrap_or(false),"max_attempts":attempts})).await;
    let mut response = match outcome {
        Err(code) => error(
            if code == "WORKSPACE_NOT_FOUND" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            },
            &code,
        ),
        Ok(value) => {
            service.wake.notify_one();
            if body["background"] == true {
                json_response(StatusCode::OK, value, "rust.dataset-reader")
            } else {
                let id = value["job_id"].clone();
                let result = tokio::time::timeout(Duration::from_secs(600), async {
                    loop {
                        let job = call(&context, "reader_job", json!({"id":id})).await?;
                        if job.is_null() {
                            return Err("READER_JOB_REMOVED".into());
                        }
                        if matches!(
                            job["status"].as_str(),
                            Some("completed" | "failed" | "stale")
                        ) {
                            let mut memory =
                                call(&context, "reader_memory", json!({"dataset_id":dataset}))
                                    .await?;
                            memory["reader_job_id"] = id.clone();
                            memory["reader_job_status"] = job["status"].clone();
                            if job["status"] == "stale" {
                                memory["reader_errors"] = json!([job["error_code"]]);
                            }
                            return Ok::<Value, String>(memory);
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
                .await;
                match result {
                    Ok(Ok(memory)) => json_response(StatusCode::OK, memory, "rust.dataset-reader"),
                    Ok(Err(code)) => error(
                        if matches!(code.as_str(), "WORKSPACE_NOT_FOUND" | "READER_JOB_REMOVED") {
                            StatusCode::NOT_FOUND
                        } else {
                            StatusCode::SERVICE_UNAVAILABLE
                        },
                        &code,
                    ),
                    Err(_) => error(
                        StatusCode::GATEWAY_TIMEOUT,
                        "Карта ещё обрабатывается. Задание сохранено; проверьте состояние позже.",
                    ),
                }
            }
        }
    };
    response.headers_mut().extend(cors);
    response
}

pub(crate) fn notify(context: &Context) {
    if let Some(service) = &context.reader {
        service.wake.notify_one();
    }
}

async fn automatic(context: &Context) -> bool {
    let state = context.state_root.clone();
    let settings = tokio::task::spawn_blocking(move || {
        crate::index_batch::configuration::Settings::load(&state)
    })
    .await;
    let Ok(Ok(settings)) = settings else {
        return false;
    };
    let yes =
        |key| ["1", "true", "yes", "on"].contains(&settings.get(key, "0").to_lowercase().as_str());
    let enabled = yes("LES_DATASET_READER_AFTER_PARSE");
    if enabled {
        if let Err(code) = call(
            context,
            "reader_reconcile",
            json!({"allow_remote":yes("LES_DATASET_READER_ALLOW_REMOTE")}),
        )
        .await
        {
            eprintln!(
                "{}",
                json!({"event":"dataset_reader_reconcile_failed","code":code})
            );
        }
    }
    enabled
}

async fn execute(context: &Arc<Context>, job: &Value) -> Result<(), String> {
    let selector = if job["model_revision"].as_str().unwrap_or("").is_empty() {
        json!({"role":"answer"})
    } else {
        json!({"revision_id":job["model_revision"]})
    };
    let saved = call(context, "model_execution_snapshot", selector).await?;
    let state = context.state_root.clone();
    let (resolved, settings) = tokio::task::spawn_blocking(move || {
        let env = std::env::var_os("LES_ENV_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| state.join(".env"));
        let resolved = model_runtime::resolve(&saved, "answer", &env)?;
        let settings = crate::index_batch::configuration::Settings::load(&state)?;
        Ok::<_, String>((resolved, settings))
    })
    .await
    .map_err(|_| "READER_MODEL_UNAVAILABLE")??;
    if resolved.endpoint.locality == "remote" && job["allow_remote"] != 1 {
        return Err("CLOUD_CONSENT_REQUIRED".into());
    }
    if resolved.endpoint.locality == "loopback"
        && les_core::system_memory::available().map_err(str::to_owned)? < 4 * 1024 * 1024 * 1024
    {
        return Err("READER_MEMORY_GUARD".into());
    }
    let bound = call(
        context,
        "reader_bind",
        json!({"id":job["id"],"revision_id":resolved.revision["revision_id"]}),
    )
    .await?;
    if bound["model_revision"] != resolved.revision["revision_id"] {
        return Err("READER_MODEL_BINDING_CHANGED".into());
    }
    let max_tokens = settings
        .get("LES_DATASET_READER_MAX_TOKENS", "768")
        .parse::<u64>()
        .map_err(|_| "READER_SETTINGS_INVALID")?
        .clamp(384, 8192);
    let limit = resolved.profile["input_token_limit"]
        .as_u64()
        .ok_or("READER_CONTEXT_LIMIT")?;
    let byte_budget = limit
        .checked_sub(max_tokens + 256)
        .ok_or("READER_CONTEXT_LIMIT")? as usize;
    let mut budget = settings
        .get("LES_DATASET_READER_CONTEXT_CHARS", "12000")
        .parse::<usize>()
        .map_err(|_| "READER_SETTINGS_INVALID")?
        .clamp(256, 64000)
        .min(byte_budget);
    let structured = les_core::model_capabilities::require(
        &resolved.snapshot,
        &["structured_output".into()],
        None,
    )
    .is_ok();
    let file_limit = match settings.get("LES_DATASET_READER_FILE_LIMIT", "").trim() {
        "" => 500,
        value => value
            .parse::<usize>()
            .map_err(|_| "READER_SETTINGS_INVALID")?
            .clamp(4, 500),
    };
    let mut prepared;
    let mut prompt;
    loop {
        prepared = call(
            context,
            "reader_prepare",
            json!({"id":job["id"],"char_limit":budget,"file_limit":file_limit}),
        )
        .await?;
        prompt = format!(
            "{}\n\nФОРМАТ ОТВЕТА:\n{}\n\nНЕДОВЕРЕННАЯ КАРТА ДОКУМЕНТОВ:\n{}",
            prepared["instruction"].as_str().unwrap_or_default(),
            prepared["schema"],
            prepared["input"]
        );
        // UTF-8 byte count is a conservative input bound; no silent token truncation.
        if prompt.len() <= byte_budget {
            break;
        }
        budget /= 2;
        if budget < 256 {
            return Err("READER_CONTEXT_LIMIT".into());
        }
    }
    let attempts = job["attempts_limit"].as_u64().unwrap_or(2).clamp(1, 3);
    if !job["repair_code"].as_str().unwrap_or_default().is_empty() {
        let repair = format!(
            "\nИсправь ответ: {}. Предыдущий JSON (может быть обрезан): {}",
            job["repair_code"],
            job["repair_output"].as_str().unwrap_or_default()
        );
        if prompt.len() + repair.len() > byte_budget {
            return Err("READER_CONTEXT_LIMIT".into());
        }
        prompt.push_str(&repair);
    }
    for attempt in job["attempts"].as_u64().unwrap_or(0) + 1..=attempts {
        call(context, "reader_attempt", json!({"id":job["id"]})).await?;
        let request = json!({"messages":[{"role":"user","content":prompt}],"max_output_tokens":max_tokens,
            "tools":[],"reasoning_enabled":false,"temperature":0,
            "response_format":if structured{json!({"type":"json_schema","json_schema":{"name":"les_dataset_reader","strict":true,"schema":prepared["schema"]}})}else{Value::Null}});
        let upstream = context.upstream_port().unwrap_or(0);
        let body = resolved
            .chat(
                &request,
                false,
                Duration::from_secs(180),
                [context.listen_port, upstream],
            )
            .await?;
        let output = tokio::time::timeout(
            Duration::from_secs(180),
            crate::model_answer::collect(body, "READER"),
        )
        .await
        .map_err(|_| "READER_MODEL_TIMEOUT")??;
        let checked = call(
            context,
            "reader_validate",
            json!({"id":job["id"],"output":output}),
        )
        .await?;
        if checked["valid"] == true {
            call(
                context,
                "reader_finish",
                json!({"id":job["id"],"output":output}),
            )
            .await?;
            return Ok(());
        }
        call(
            context,
            "reader_retry",
            json!({"id":job["id"],"code":checked["code"],"output":output}),
        )
        .await?;
        if attempt == attempts {
            return Err(checked["code"]
                .as_str()
                .unwrap_or("READER_OUTPUT_INVALID")
                .into());
        }
        let repair = format!(
            "\nИсправь ответ: {}. Предыдущий JSON (может быть обрезан): {}",
            checked["code"],
            output.chars().take(500).collect::<String>()
        );
        if prompt.len() + repair.len() > byte_budget {
            return Err("READER_CONTEXT_LIMIT".into());
        }
        prompt.push_str(&repair);
    }
    Err("READER_ATTEMPTS_EXHAUSTED".into())
}

fn lease(state: &Path) -> Option<File> {
    let folder = state.join("data");
    std::fs::create_dir_all(&folder).ok()?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(folder.join("native-dataset-reader.lock"))
        .ok()?;
    lock.try_lock().ok()?;
    Some(lock)
}
pub async fn run(context: Arc<Context>) {
    let state = context.state_root.clone();
    let Ok(Some(_lease)) = tokio::task::spawn_blocking(move || lease(&state)).await else {
        return;
    };
    if call(&context, "reader_recover", json!({})).await.is_err() {
        return;
    }
    loop {
        let enabled = automatic(&context).await;
        if let Ok(job) = call(&context, "reader_claim", json!({"auto_enabled":enabled})).await {
            if job["id"].is_string() {
                eprintln!(
                    "{}",
                    json!({"event":"dataset_reader_started","job_id":job["id"],"recoveries":job["recoveries"]})
                );
                if let Err(cause) = execute(&context, &job).await {
                    let raw = cause.split(':').next().unwrap_or("READER_MODEL_FAILED");
                    let code = if raw
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    {
                        raw
                    } else {
                        "READER_MODEL_FAILED"
                    };
                    let _ = call(
                        &context,
                        "reader_finish",
                        json!({"id":job["id"],"error_code":code}),
                    )
                    .await;
                    eprintln!(
                        "{}",
                        json!({"event":"dataset_reader_failed","job_id":job["id"],"code":code})
                    );
                } else {
                    eprintln!(
                        "{}",
                        json!({"event":"dataset_reader_finished","job_id":job["id"]})
                    );
                }
                continue;
            }
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            context.reader.as_ref().unwrap().wake.notified(),
        )
        .await;
    }
}
