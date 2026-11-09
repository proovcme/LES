//! Read model connections directly from the native transaction owner.
//! Writes and capability probes still use the retained HTTP handlers.
mod discovery;
use crate::{auth, error, json_response, Context};
use axum::{
    extract::Request,
    http::{header, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use std::{collections::HashSet, io::Read, path::Path, sync::Arc, time::Duration};

pub fn handles(method: &str, path: &str) -> bool {
    discovery::handles(method, path)
        || (method == "GET"
            && matches!(
                path,
                "/api/model-connections"
                    | "/api/model-connections/templates"
                    | "/api/model-connections/effective"
            ))
}

fn secret_key(reference: &str) -> Option<&str> {
    let key = reference.trim().strip_prefix("env:")?;
    let migrated = [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "OLLAMA_API_KEY",
        "LEMONADE_API_KEY",
        "FREETOKEN_API_KEY",
    ]
    .contains(&key);
    let connection = key
        .strip_prefix("LES_MODEL_CONNECTION_")
        .and_then(|v| v.strip_suffix("_API_KEY"))
        .is_some_and(|v| {
            !v.is_empty()
                && v.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        });
    (migrated || connection).then_some(key)
}

fn configured_keys(path: &Path) -> Result<HashSet<String>, ()> {
    let mut content = String::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut content)
                .map_err(|_| ())?;
            if content.len() > 8 * 1024 * 1024 {
                return Err(());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(()),
    }
    // Match the retained store: only the first occurrence of a key counts.
    let mut seen = HashSet::new();
    let mut present = HashSet::new();
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if seen.insert(key.to_string()) && !value.trim().is_empty() {
                present.insert(key.to_string());
            }
        }
    }
    Ok(present)
}

fn project(mut value: Value, env_path: &Path) -> Result<Value, ()> {
    let mut keys: Option<HashSet<String>> = None;
    for row in value["connections"].as_array_mut().ok_or(())? {
        let status = match row["secret_ref"].as_str() {
            None if row["secret_ref"].is_null() => "not_required",
            Some(reference) => match secret_key(reference) {
                None => "invalid_reference",
                Some(key) => {
                    if std::env::var(key).is_ok_and(|v| !v.trim().is_empty()) {
                        "configured"
                    } else {
                        if keys.is_none() {
                            keys = Some(configured_keys(env_path)?);
                        }
                        if keys.as_ref().unwrap().contains(key) {
                            "configured"
                        } else {
                            "missing"
                        }
                    }
                }
            },
            _ => return Err(()),
        };
        let snapshot = row
            .as_object_mut()
            .ok_or(())?
            .remove("snapshot")
            .ok_or(())?;
        let observations = if snapshot.is_null() {
            json!([])
        } else {
            let raw = snapshot["observations_json"].as_str().ok_or(())?;
            let mut items: Value = serde_json::from_str(raw).map_err(|_| ())?;
            for item in items.as_array_mut().ok_or(())? {
                let object = item.as_object_mut().ok_or(())?;
                let name = object.remove("capability").ok_or(())?;
                object.insert("name".into(), name);
                object.entry("detail").or_insert(json!(""));
            }
            items
        };
        let enabled = row["enabled"].as_i64().ok_or(())? != 0;
        let object = row.as_object_mut().ok_or(())?;
        object.remove("secret_ref");
        object.insert("enabled".into(), json!(enabled));
        object.insert("secret_status".into(), json!(status));
        object.insert("capabilities".into(), observations);
    }
    Ok(value)
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if discovery::handles(request.method().as_str(), request.uri().path()) {
        return discovery::handle(context, request).await;
    }
    let effective = request.uri().path() == "/api/model-connections/effective";
    let cors = match auth::authorize(&context, request.headers(), !effective).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let mut response = if request.uri().path().ends_with("/templates") {
        match serde_json::from_str(include_str!("../../config/model-connection-templates.json")) {
            Ok(value) => json_response(StatusCode::OK, value, "rust.model-registry"),
            Err(_) => error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Не удалось прочитать шаблоны подключений.",
            ),
        }
    } else {
        let permit = match tokio::time::timeout(
            Duration::from_secs(3),
            context.workspace_slots.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            _ => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Реестр моделей занят. Повторите действие.",
                )
            }
        };
        let database = context.database.clone();
        let env_path = std::env::var_os("LES_ENV_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| context.state_root.join(".env"));
        match tokio::task::spawn_blocking(move || {
            let _permit=permit;
            // Idempotent schema initialization preserves existing IDs, bindings and audit rows.
            for action in ["model_registry_init","model_registry_view"] {
                let result=les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,"action":action,"arguments":{}}));
                if result["status"]!="completed" { return Err(()); }
                if action=="model_registry_view" {
                    if effective {
                        let result=les_core::catalog::execute(&json!({"schema":"les.catalog.v1","database":database,"action":"model_execution_roles","arguments":{}}));
                        if result["status"]!="completed" {return Err(());}
                        let mut roles=serde_json::Map::new();
                        for role in ["answer","embeddings","local_fallback"] {
                            let execution=&result["value"][role];
                            let value=if execution.is_null(){Value::Null}else{
                                match crate::model_runtime::resolve(execution,role,&env_path) {
                                    Ok(resolved)=>resolved.public(),
                                    Err(code)=>{
                                        let r=&execution["revision"];
                                        json!({"connection_id":r["connection_id"],"revision_id":r["revision_id"],"display_name":r["display_name"],"model_id":r["model_id"],"locality":r["locality"],"status":"blocked","code":code.split(':').next().unwrap_or(&code)})
                                    }
                                }
                            };
                            roles.insert(role.into(),value);
                        }
                        return Ok(json!({"roles":roles}));
                    }
                    return project(result["value"].clone(),&env_path);
                }
            }
            Err(())
        }).await {
            Ok(Ok(value))=>json_response(StatusCode::OK,value,"rust.model-registry"),
            _=>error(StatusCode::SERVICE_UNAVAILABLE,"Не удалось прочитать подключения моделей. Настройки сохранены; повторите загрузку."),
        }
    };
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_are_allowlisted_and_never_exposed_as_values() {
        assert_eq!(secret_key("env:OPENAI_API_KEY"), Some("OPENAI_API_KEY"));
        assert_eq!(
            secret_key("env:LES_MODEL_CONNECTION_A_01_API_KEY"),
            Some("LES_MODEL_CONNECTION_A_01_API_KEY")
        );
        for reference in [
            "env:PATH",
            "file:key",
            "env:LES_MODEL_CONNECTION__API_KEY",
            "env:LES_MODEL_CONNECTION_a_API_KEY",
        ] {
            assert_eq!(secret_key(reference), None);
        }
    }
}
