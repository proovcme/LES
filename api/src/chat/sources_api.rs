//! Model-independent ingress to the chat's canonical source tools.
use super::{
    tools::sources::{self, SourceContext},
    unwrap, Result,
};
use crate::Context;
use axum::{
    body::to_bytes,
    extract::Request,
    http::{header, Method, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

pub const PATH: &str = "/api/rag/sources/call";
pub const TOOLS_PATH: &str = "/api/tools/call";
const ALLOWED: &[&str] = &[
    "search_sources",
    "read_source",
    "read_pdf_source",
    "read_excel_source",
    "dataset_map",
];

fn validate(context: &Context, name: &str, args: &Value) -> Result<()> {
    let validated = unwrap(les_core::tool_registry::execute(&json!({
        "schema":"les.tool-registry.v1", "database":context.database,
        "action":"validate", "name":name, "arguments":args
    })))?;
    if validated["valid"] != true {
        return Err("TOOL_SCHEMA_INVALID".into());
    }
    Ok(())
}
fn prepare(context: &Context, input: &Value) -> Result<(String, Value, SourceContext)> {
    let name = input["tool"]
        .as_str()
        .filter(|n| ALLOWED.contains(n))
        .ok_or("TOOL_NOT_ALLOWED")?;
    let args = input["args"].clone();
    validate(context, name, &args)?;
    if matches!(name, "search_sources" | "read_source" | "read_excel_source")
        && args["offset"].as_u64().is_some_and(|n| n > 0)
        && args["view"] != "documents"
        && args["view"] != "parts"
        && args["index_revision"].as_str().is_none_or(str::is_empty)
    {
        return Err("SOURCE_CONTINUATION_REVISION_REQUIRED".into());
    }
    let catalog = unwrap(crate::library::catalog(
        &context.database,
        "library_datasets",
        json!({}),
    ))?;
    let scope = source_scope(&args, &catalog)?;
    if name == "search_sources" && args["view"] != "documents" {
        let selected = scope["resolved_dataset_ids"]
            .as_array()
            .ok_or("TOOL_SCOPE_VIOLATION")?;
        let has_chunks = catalog["datasets"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| {
                selected.contains(&row["id"]) && row["chunk_count"].as_u64().is_some_and(|n| n > 0)
            });
        if !has_chunks {
            return Err("SOURCE_INDEX_NOT_READY".into());
        }
    }
    let request = json!({"question":args["q"]});
    Ok((
        name.into(),
        args,
        SourceContext {
            request,
            scope,
            profile: Value::Null,
        },
    ))
}
fn source_scope(args: &Value, catalog: &Value) -> Result<Value> {
    if let (Some(id), Some(ids)) = (args["dataset_id"].as_str(), args["dataset_ids"].as_array()) {
        if !ids.contains(&json!(id)) {
            return Err("TOOL_SCOPE_VIOLATION".into());
        }
    }
    let datasets: Vec<Value> = catalog["datasets"]
        .as_array()
        .ok_or("SOURCE_CATALOG_UNAVAILABLE")?
        .iter()
        .filter(|row| row["dataset_scope"] != "system")
        .cloned()
        .collect();
    let requested = if let Some(id) = args["dataset_id"].as_str() {
        json!([id])
    } else {
        args["dataset_ids"].clone()
    };
    // Public source tools accept registered IDs, not the legacy chat's name
    // aliases: two datasets may share a display name.
    if requested.as_array().is_some_and(|ids| {
        ids.iter()
            .any(|id| !datasets.iter().any(|row| row["id"] == *id))
    }) {
        return Err("TOOL_SCOPE_VIOLATION".into());
    }
    // An explicit empty set means no sources, never all datasets.
    let scope_request = json!({"scope":{"scope_type":if requested.is_null(){"all"}else{"datasets"},"dataset_ids":requested}});
    let scope = les_core::chat_request::resolve_scope(&scope_request, &datasets, &[])?;
    if scope["warnings"].as_array().is_some_and(|v| !v.is_empty()) {
        return Err("TOOL_SCOPE_VIOLATION".into());
    }
    if scope["resolved_dataset_ids"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        return Err("CHAT_SCOPE_EMPTY".into());
    }
    Ok(scope)
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let public_tools = request.uri().path() == TOOLS_PATH;
    let cors = match crate::auth::authorize(&context, request.headers(), false).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if request.method() != Method::POST {
        let mut reply = crate::error(StatusCode::METHOD_NOT_ALLOWED, "Используйте POST.");
        reply
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
        reply.headers_mut().extend(cors);
        return reply;
    }
    let result: Result<Value> = async {
        if request.method() != Method::POST {
            return Err("TOOL_SCHEMA_INVALID".into());
        }
        let _permit = context
            .workspace_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "CHAT_SERVICE_BUSY")?;
        let body = to_bytes(request.into_body(), 65536)
            .await
            .map_err(|_| "TOOL_SCHEMA_INVALID")?;
        let input: Value = serde_json::from_slice(&body).map_err(|_| "TOOL_SCHEMA_INVALID")?;
        if public_tools && matches!(input["tool"].as_str(), Some("web_search" | "web_read")) {
            let owner = context.clone();
            let checked = input.clone();
            tokio::task::spawn_blocking(move || {
                validate(&owner, checked["tool"].as_str().unwrap(), &checked["args"])
            })
            .await
            .map_err(|_| "TOOL_SCHEMA_INVALID")??;
            let mut value = input["args"].clone();
            value["schema"] = json!("les.web.v1");
            value["operation"] = json!(if input["tool"] == "web_search" {
                "search"
            } else {
                "read"
            });
            return Ok(crate::web::execute(&value).await);
        }
        let owner = context.clone();
        let (name, args, turn) = tokio::task::spawn_blocking(move || prepare(&owner, &input))
            .await
            .map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
        tokio::time::timeout(
            Duration::from_secs(150),
            sources::run_scoped(context.clone(), &turn, &name, args),
        )
        .await
        .map_err(|_| "TOOL_DEADLINE_EXCEEDED")?
    }
    .await;
    let mut response = match result {
        Ok(value) => crate::json_response(StatusCode::OK, value, "rust.sources"),
        Err(e) => {
            eprintln!("{}", json!({"event":"source_tool_failed","code":e.code}));
            let status = match e.code.as_str() {
                "TOOL_SCHEMA_INVALID" | "SOURCE_CONTINUATION_REVISION_REQUIRED" => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                "TOOL_SCOPE_VIOLATION" | "TOOL_NOT_ALLOWED" => StatusCode::FORBIDDEN,
                "SOURCE_DOCUMENT_NOT_FOUND" | "SOURCE_DOCUMENT_MISSING" => StatusCode::NOT_FOUND,
                "SOURCE_REVISION_CHANGED"
                | "PDF_REVISION_CHANGED"
                | "INDEX_CHANGED_DURING_SEARCH" => StatusCode::CONFLICT,
                _ => e.status,
            };
            crate::json_response(status, e.payload(), "rust.sources")
        }
    };
    response.headers_mut().extend(cors);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_scope_is_closed_and_does_not_expand_empty_or_hidden_sets() {
        let catalog = json!({"datasets":[{"id":"public","name":"Display alias"},{"id":"hidden","dataset_scope":"system"}]});
        assert_eq!(
            source_scope(&json!({}), &catalog).unwrap()["resolved_dataset_ids"],
            json!(["public"])
        );
        for args in [
            json!({"dataset_ids":[]}),
            json!({"dataset_ids":["hidden"]}),
            json!({"dataset_ids":["public","unknown"]}),
            json!({"dataset_ids":["Display alias"]}),
        ] {
            assert!(source_scope(&args, &catalog).is_err());
        }
    }
}
