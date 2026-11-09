use super::{unwrap, Context, Result};
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
}
fn catalog(database: &Path, action: &str, args: Value) -> Result<Value> {
    unwrap(crate::library::catalog(database, action, args))
}
pub async fn prepare(context: Arc<Context>, input: Value) -> Result<Prepared> {
    let owner = context.clone();
    let mut turn = tokio::task::spawn_blocking(move || prepare_sync(&owner, input))
        .await
        .map_err(|_| "CHAT_PREPARATION_FAILED")??;
    if let Some(id) = turn.request["attachment_id"].as_str() {
        let attachment = crate::attachments::bound(context, id.to_owned())
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
pub(crate) fn answer(context: &Context) -> Result<model_runtime::Resolved> {
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
    if context.public_chat_rust && execution["revision"]["locality"] == "remote" {
        return Err("NATIVE_CLOUD_POLICY_PENDING".into());
    }
    let settings = crate::index_batch::configuration::Settings::load(&context.state_root)?;
    let env = std::env::var_os("LES_ENV_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    let connection = model_runtime::resolve(&execution, "answer", &env)?;
    if connection.endpoint.locality == "remote"
        && !matches!(
            settings
                .get("LES_CLOUD_CONSENT", "false")
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "on"
        )
    {
        return Err("CLOUD_CONSENT_REQUIRED".into());
    }
    if connection.endpoint.locality != "remote"
        && les_core::system_memory::available()? < 4 * 1024 * 1024 * 1024
    {
        return Err("CHAT_MEMORY_GUARD".into());
    }
    Ok(connection)
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
fn prepare_sync(context: &Context, request: Value) -> Result<Prepared> {
    let database = &context.database;
    let connection = answer(context)?;
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
        memory.push(json!({"object_id":"agent-file:context","payload":projection?}));
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
    })
}
