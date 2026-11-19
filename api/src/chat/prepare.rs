use super::{unwrap, Context, Result, Sink};
use crate::model_runtime;
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};
pub struct Prepared {
    pub request: Value,
    pub scope: Value,
    pub profile: Value,
    pub connection: model_runtime::Resolved,
    pub session: Value,
    pub dialogue: Vec<Value>,
    pub memory: Vec<Value>,
    pub attachment: Value,
    pub attachment_owner: Option<String>,
    pub egress_tag: Option<String>,
}
fn catalog(database: &Path, action: &str, args: Value) -> Result<Value> {
    unwrap(crate::library::catalog(database, action, args))
}
pub async fn prepare(
    context: Arc<Context>,
    input: Value,
    caller: Option<String>,
    sink: &Sink,
) -> Result<Prepared> {
    let mut turn = match prepare_once(context.clone(), input.clone(), caller.clone()).await {
        Err(error) if capability_needs_refresh(&error.code) => {
            refresh_local_answer(context.clone(), sink).await?;
            prepare_once(context.clone(), input, caller).await?
        }
        other => other?,
    };
    if let Some(id) = turn.request["attachment_id"].as_str() {
        let attachment =
            crate::attachments::bound(context, id.to_owned(), turn.attachment_owner.clone())
                .await
                .map_err(|code| super::error::Failure::new(&code))?;
        turn.request["attachment_context"] = if attachment["media_type"] == "image" {
            json!(format!(
                "Прикреплено изображение: {} ({} × {}).",
                attachment["name"].as_str().unwrap_or("image"),
                attachment["width"],
                attachment["height"]
            ))
        } else {
            attachment["text"].clone()
        };
        turn.attachment = attachment;
    }
    Ok(turn)
}
async fn prepare_once(
    context: Arc<Context>,
    input: Value,
    caller: Option<String>,
) -> Result<Prepared> {
    tokio::task::spawn_blocking(move || prepare_sync(&context, input, caller.as_deref()))
        .await
        .map_err(|_| "CHAT_PREPARATION_FAILED")?
}

fn capability_needs_refresh(code: &str) -> bool {
    matches!(
        code,
        "CAPABILITY_SNAPSHOT_STALE" | "CAPABILITY_SNAPSHOT_MISSING"
    )
}

pub(crate) fn can_refresh_answer(revision: &Value, code: &str) -> bool {
    revision["locality"] == "loopback" && capability_needs_refresh(code)
}

async fn refresh_local_answer(context: Arc<Context>, sink: &Sink) -> Result<()> {
    let owner = context.clone();
    let initial = tokio::task::spawn_blocking(move || answer_execution(&owner))
        .await
        .map_err(|_| "CHAT_PREPARATION_FAILED")??;
    if initial["revision"]["locality"] != "loopback" {
        return resolve_answer(&context, &initial).map(|_| ());
    }

    let slot = &context.chat.get().ok_or("CHAT_SERVICE_BUSY")?.local;
    let _permit = tokio::time::timeout(std::time::Duration::from_secs(30), slot.acquire())
        .await
        .map_err(|_| "MODEL_QUEUE_TIMEOUT")?
        .map_err(|_| "MODEL_QUEUE_TIMEOUT")?;
    let owner = context.clone();
    let pending = tokio::task::spawn_blocking(move || {
        let execution = answer_execution(&owner)?;
        match resolve_answer(&owner, &execution) {
            Ok(_) => Ok(None),
            Err(error) if can_refresh_answer(&execution["revision"], &error.code) => {
                Ok(Some(execution["revision"].clone()))
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(|_| "CHAT_PREPARATION_FAILED")??;
    let Some(revision) = pending else {
        return Ok(());
    };
    let connection = revision["connection_id"]
        .as_str()
        .ok_or("CHAT_ANSWER_PROBE_FAILED")?;
    sink.progress("model_check", "Проверяю подключение локальной модели")
        .await?;
    crate::model_registry::probe::run(
        context.clone(),
        connection,
        "chat:local-answer",
        &json!({"revision_id":revision["revision_id"],
            "capabilities":["chat_completions","streaming","tools","structured_output"]}),
        crate::model_registry::probe::Mode::LocalAnswer,
    )
    .await?;
    // Ollama acknowledges keep_alive=0 before Windows necessarily reclaims
    // the runner's memory. Give that shutdown a chance without lowering the gate.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    if les_core::system_memory::available()? < 4 * 1024 * 1024 * 1024 {
        sink.progress(
            "model_check_wait",
            "Жду освобождения памяти после проверки модели",
        )
        .await?;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if les_core::system_memory::available()? >= 4 * 1024 * 1024 * 1024 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("CHAT_MEMORY_GUARD".into());
            }
        }
    }
    Ok(())
}
pub(crate) fn answer(context: &Context) -> Result<model_runtime::Resolved> {
    let execution = answer_execution(context)?;
    if context.public_chat_rust && execution["revision"]["locality"] == "remote" {
        return Err("CLOUD_SUMMARY_LOCAL_ONLY".into());
    }
    resolve_answer(context, &execution)
}
fn answer_execution(context: &Context) -> Result<Value> {
    let database = &context.database;
    catalog(database, "model_registry_init", json!({}))?;
    let execution = catalog(
        database,
        "model_execution_snapshot",
        json!({"role":"answer"}),
    )?;
    if execution.is_null() {
        return Err("ANSWER_MODEL_NOT_ASSIGNED".into());
    }
    Ok(execution)
}
fn resolve_answer(context: &Context, execution: &Value) -> Result<model_runtime::Resolved> {
    let settings = crate::index_batch::configuration::Settings::load(&context.state_root)?;
    let env = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    if execution["revision"]["locality"] == "remote" && !super::egress::consent(&settings) {
        return Err("CLOUD_CONSENT_REQUIRED".into());
    }
    let connection = model_runtime::resolve(execution, "answer", &env)?;
    if connection.endpoint.locality != "remote"
        && les_core::system_memory::available()? < 4 * 1024 * 1024 * 1024
    {
        return Err("CHAT_MEMORY_GUARD".into());
    }
    Ok(connection)
}

pub(crate) fn admission(context: &Context) -> Value {
    let result =
        answer_execution(context).and_then(|execution| resolve_answer(context, &execution));
    let memory = les_core::system_memory::available()
        .ok()
        .map(|bytes| bytes as f64 / 1_073_741_824.0);
    match result {
        Ok(_) => json!({"allowed":true,"memory":{"ram_free_gb":memory}}),
        Err(error) => {
            json!({"allowed":false,"code":error.code,"reason":error.payload()["detail"],"memory":{"ram_free_gb":memory}})
        }
    }
}
pub(super) fn admit(request: &Value) -> Result<()> {
    if request["provider_config"].is_object() {
        return Err("SESSION_PROVIDER_OVERRIDE_DISABLED".into());
    }
    if request["candidate_acceptance"] == true {
        return Err("CANDIDATE_ACCEPTANCE_REQUIRED".into());
    }
    // Reject unsupported requests before creating a receipt or calling a model,
    // including deterministic commands, with no mid-turn fallback.
    for (field, code) in [
        ("validation_enabled", "NATIVE_VALIDATION_CAPABILITY_PENDING"),
        ("semantic_cache_enabled", "NATIVE_CACHE_CAPABILITY_PENDING"),
        ("reranker_enabled", "NATIVE_RERANKER_CAPABILITY_PENDING"),
    ] {
        if request[field] == true {
            return Err(code.into());
        }
    }
    Ok(())
}
fn prepare_sync(context: &Context, request: Value, caller: Option<&str>) -> Result<Prepared> {
    let database = &context.database;
    let execution = answer_execution(context)?;
    let connection = resolve_answer(context, &execution)?;
    let attachment_owner = caller.map(|v| les_core::sha256_hex(v.as_bytes()));
    let sid = request["session_id"].as_str().unwrap_or("");
    let session = if sid.is_empty() {
        Value::Null
    } else {
        unwrap(les_core::workspace::execute(
            &json!({"schema":"les.workspace.v1","database":database,"action":"get","arguments":{"session_id":sid}}),
        ))?
    };
    let profile = unwrap(les_core::chat_profiles::execute(
        &json!({"schema":"les.chat-profiles.v1","database":database,"action":"resolve",
        "arguments":{"session_id":request["session_id"],"requested_mode":request["mode"],"requested_revision_id":request["profile_revision_id"],"apply_revision":request["apply_profile_revision"]==true}}),
    ))?;
    let needs_scope = request["scope"]["scope_type"]
        .as_str()
        .is_some_and(|s| s != "none")
        || request["dataset_ids"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        || request["project_id"].as_i64().is_some_and(|n| n > 0)
        || request["dataset_filter"]
            .as_str()
            .is_some_and(|s| !s.is_empty());
    let datasets = if needs_scope {
        catalog(database, "library_datasets", json!({}))?["datasets"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        vec![]
    };
    let projects = if needs_scope {
        let mut ids = request["scope"]["project_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(id) = request["project_id"].as_i64().filter(|v| *v > 0) {
            if !ids.contains(&json!(id)) {
                ids.push(json!(id));
            }
        }
        let mut projected = Vec::new();
        for id in ids {
            let id = id
                .as_i64()
                .or_else(|| id.as_str()?.trim().parse().ok())
                .filter(|v| *v > 0)
                .ok_or("CHAT_SCOPE_INVALID")?;
            let project = catalog(database, "project_get", json!({"project_id":id}))?;
            if project.is_null() {
                continue;
            }
            let links = catalog(
                database,
                "project_links",
                json!({"project_id":id,"kind":"dataset"}),
            )?;
            projected.push(json!({"id":id,"dataset_ids":links.as_array().into_iter().flatten().filter_map(|v|v.get("ref")).collect::<Vec<_>>()}));
        }
        projected
    } else {
        vec![]
    };
    let scope = les_core::chat_request::resolve_scope(&request, &datasets, &projects)?;
    if scope["document_grounding_enabled"] == true
        && scope["resolved_dataset_ids"]
            .as_array()
            .is_none_or(Vec::is_empty)
    {
        return Err("CHAT_SCOPE_EMPTY".into());
    }
    let egress_tag = if context.public_chat_rust && execution["revision"]["locality"] == "remote" {
        Some(super::egress::admit(
            context,
            &request,
            &scope,
            &execution["revision"],
            attachment_owner.as_deref(),
        )?)
    } else {
        None
    };
    let context_state = catalog(database, "context_get", json!({"session_id":sid}))?;
    let history = if context_state["enabled"] != false && !sid.is_empty() {
        catalog(
            database,
            "context_history",
            json!({"session_id":sid,"limit":40,"after_id":context_state["cutoff_id"].as_i64().unwrap_or(0).max(context_state["through_id"].as_i64().unwrap_or(0))}),
        )?
    } else {
        json!([])
    };
    if let Some(tag) = &egress_tag {
        les_core::chat_request::egress::history(database, sid, &history, tag)?;
    }
    let mut dialogue = Vec::new();
    for row in history.as_array().into_iter().flatten() {
        for (field, role) in [("question", "user"), ("answer", "assistant")] {
            if let Some(text) = row[field].as_str().filter(|s| !s.is_empty()) {
                dialogue.push(json!({"role":role,"content":text}));
            }
        }
    }
    let mut memory = Vec::new();
    if context_state["enabled"] != false
        && context_state["summary"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    {
        memory.push(json!({"object_id":"conversation-summary:current","payload":{"summary":context_state["summary"],"advisory":true}}));
    }
    if session["registered"] == true {
        let projection = unwrap(les_core::agent_files::execute(
            &json!({"schema":"les.agent-files.v1","database":database,"state_root":context.state_root,
            "action":"projection","arguments":{"project_id":session["project_id"].as_i64().unwrap_or(0)}}),
        ));
        let projection = projection?;
        if egress_tag.is_none()
            || projection["records"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        {
            memory.push(json!({"object_id":"agent-file:context","payload":projection}));
        }
    }
    if egress_tag.is_some() && !memory.is_empty() {
        return Err("CLOUD_MEMORY_LOCAL_ONLY".into());
    }
    Ok(Prepared {
        request,
        scope,
        profile,
        connection,
        session,
        dialogue,
        memory,
        attachment: Value::Null,
        attachment_owner,
        egress_tag,
    })
}
