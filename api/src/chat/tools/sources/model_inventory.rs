//! Exact dataset reads for engineering tables by source identity.
mod dwg;
mod dwg_spec;
mod dwg_tables;
mod rows;
mod rvt_counts;

use super::{
    document, envelope, reference, scoped, scoped_document, selected_scoped, SourceContext,
};
use crate::{
    chat::{unwrap, Context, Result},
    index_batch::configuration::{identity, Settings},
};
use les_core::engineering::{
    indexed_record_id, indexed_record_key, DWG_RECORD_SCHEMA, DWG_TABLE_ROW_SCHEMA, RECORD_SCHEMA,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

const TOOL: &str = "model_inventory";
const MAX_MODELS: usize = 32;
const MAX_RECORDS: usize = 50_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const CHANGED: &str = "INDEX_CHANGED_DURING_SEARCH";
const LIMIT: &str = "MODEL_INVENTORY_LIMIT";

fn format(args: &Value) -> &'static str {
    if matches!(
        args["mode"].as_str(),
        Some("dwg_blocks" | "dwg_spec" | "dwg_tables")
    ) {
        "dwg"
    } else {
        "rvt"
    }
}

fn matches_format(doc: &Value, format: &str) -> bool {
    doc["file_name"]
        .as_str()
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(format))
}

/// Offer the tool from the selected source metadata, independently of the question.
pub(in crate::chat) async fn available(context: &Context, turn: &SourceContext) -> Result<bool> {
    let database = context.database.clone();
    let scope = turn.scope["resolved_dataset_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids = turn.request["scope"]["document_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let names = selected_scoped(turn);
    let indexed_dwg = context.index_store.is_some();
    let supported = move |doc: &Value| {
        (matches_format(doc, "rvt") && doc["status"] != "MISSING")
            || (indexed_dwg && doc["status"] == "INDEXED" && matches_format(doc, "dwg"))
    };
    if !ids.is_empty() {
        for id in ids {
            let doc = document(context, &json!({"doc_id":id})).await?;
            if scope.contains(&doc["dataset_id"])
                && (names.is_empty() || names.contains(&doc["file_name"]))
                && supported(&doc)
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    tokio::task::spawn_blocking(move || -> Result<bool> {
        for dataset in scope {
            for format in ["dwg", "rvt"] {
                let mut offset = 0;
                loop {
                    let result = unwrap(crate::library::catalog(&database, "library_documents", json!({
                        "dataset_id":dataset,"q":format!(".{format}"),"doc_filter":names,"offset":offset,"limit":100
                    })))?;
                    let rows = result["documents"].as_array().ok_or("SOURCE_CATALOG_UNAVAILABLE")?;
                    if rows.iter().any(supported) {
                        return Ok(true);
                    }
                    offset += rows.len();
                    if rows.is_empty() || offset as u64>=result["total"].as_u64().unwrap_or(0) {
                        break;
                    }
                }
            }
        }
        Ok(false)
    }).await.map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")?
}

pub(in crate::chat) async fn direct_rvt_documents(
    context: &Context,
    turn: &SourceContext,
) -> Result<Vec<Value>> {
    Ok(models(context, turn, &json!({"mode":"rvt_counts"}))
        .await?
        .into_iter()
        .filter(|doc| doc["status"] != "MISSING")
        .map(|doc| json!({"doc_id":doc["id"],"file_name":doc["file_name"]}))
        .collect())
}

async fn models(context: &Context, turn: &SourceContext, args: &Value) -> Result<Vec<Value>> {
    let format = format(args);
    let mut ids = turn.request["scope"]["document_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let scope = turn.scope["resolved_dataset_ids"]
        .as_array()
        .ok_or("CHAT_SCOPE_EMPTY")?;
    let datasets: Vec<_> = if args["dataset_id"].is_string() {
        vec![args["dataset_id"].clone()]
    } else {
        args["dataset_ids"].as_array().unwrap_or(scope).clone()
    };
    if datasets.is_empty() {
        return Err("CHAT_SCOPE_EMPTY".into());
    }
    for dataset in &datasets {
        scoped(turn, dataset)?;
    }
    if let Some(id) = args["doc_id"].as_str() {
        if !ids.is_empty() && !ids.iter().any(|v| v == id) {
            return Err("TOOL_SCOPE_VIOLATION".into());
        }
        ids = vec![json!(id)];
    }
    let files = selected_scoped(turn);
    if ids.is_empty() {
        let database = context.database.clone();
        let requested = datasets.clone();
        let names = files.clone();
        ids = tokio::task::spawn_blocking(move || -> Result<Vec<Value>> {
            let mut ids = Vec::new();
            for dataset in requested {
                let mut offset = 0;
                loop {
                    let page = unwrap(crate::library::catalog(&database, "library_documents", json!({
                       "dataset_id":dataset,"q":format!(".{format}"),"doc_filter":names,"limit":100,"offset":offset
                    })))?;
                    let docs = page["documents"].as_array().ok_or("SOURCE_CATALOG_UNAVAILABLE")?;
                    ids.extend(docs.iter().filter(|doc| matches_format(doc, format)).map(|doc| doc["id"].clone()));
                    if ids.len() > MAX_MODELS {
                        return Err(LIMIT.into());
                    }
                    offset += docs.len();
                    if docs.is_empty() || offset as u64 >= page["total"].as_u64().unwrap_or(0) {
                        break;
                    }
                }
            }
            Ok(ids)
        }).await.map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
    }
    let ids: BTreeSet<_> = ids
        .into_iter()
        .map(|id| id.as_str().unwrap_or_default().to_owned())
        .collect();
    if ids.len() > MAX_MODELS {
        return Err(LIMIT.into());
    }
    let mut docs = Vec::new();
    for id in ids {
        let doc = document(context, &json!({"doc_id":id})).await?;
        scoped(turn, &doc["dataset_id"])?;
        if !datasets.contains(&doc["dataset_id"])
            || (!files.is_empty() && !files.contains(&doc["file_name"]))
        {
            return Err("TOOL_SCOPE_VIOLATION".into());
        }
        if matches_format(&doc, format) {
            docs.push(doc);
        }
    }
    Ok(docs)
}

struct ModelRows {
    doc: Value,
    records: Vec<Value>,
}

pub(in crate::chat) async fn run(
    context: Arc<Context>,
    turn: &SourceContext,
    args: &Value,
) -> Result<Value> {
    if args["offset"].as_u64().is_some_and(|offset| offset > 0)
        && args["index_revision"].as_str().is_none_or(str::is_empty)
    {
        return Err("SOURCE_CONTINUATION_REVISION_REQUIRED".into());
    }
    let mut docs = models(&context, turn, args).await?;
    if args["mode"] == "rvt_counts" {
        if docs.is_empty() {
            return Err("MODEL_INVENTORY_NO_RVT".into());
        }
        if docs.len() != 1 {
            return Err("MODEL_INVENTORY_SELECT_RVT_DOCUMENT".into());
        }
        let doc = docs.pop().unwrap();
        scoped_document(turn, &doc)?;
        return rvt_counts::run(context, doc, args.clone()).await;
    }
    scan(context, turn, docs, args).await
}

async fn scan(
    context: Arc<Context>,
    turn: &SourceContext,
    docs: Vec<Value>,
    args: &Value,
) -> Result<Value> {
    let format = format(args);
    let dwg = format == "dwg";
    let (schema, sections): (_, &[&str]) = match args["mode"].as_str() {
        Some("dwg_tables") => (DWG_TABLE_ROW_SCHEMA, &["dwg_table_rows"]),
        _ if dwg => (DWG_RECORD_SCHEMA, &["elements"]),
        _ => (RECORD_SCHEMA, &["elements", "types"]),
    };
    if docs.is_empty() {
        return Err(if dwg {
            "MODEL_INVENTORY_NO_DWG"
        } else {
            "MODEL_INVENTORY_NO_RVT"
        }
        .into());
    }
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let (collection, _) = identity(&context, &Settings::load(&context.state_root)?)?;
    let revision = store.revision(&context, &collection).await?;
    if args["index_revision"].is_string() && args["index_revision"] != revision {
        return Err(CHANGED.into());
    }
    let mut seen = HashSet::new();
    let mut bytes = 0;
    let mut models = Vec::new();
    for doc in docs {
        let mut records = Vec::new();
        if doc["status"] == "INDEXED" {
            scoped_document(turn, &doc)?;
            let ds = doc["dataset_id"].as_str().ok_or(CHANGED)?;
            let file = doc["file_name"].as_str().ok_or(CHANGED)?;
            let sha = les_core::engineering::digest(&doc["file_hash"]).map_err(|_| CHANGED)?;
            let mut cursor = Value::Null;
            loop {
                let mut request = json!({"limit":64,"with_vectors":false,
                    "with_payload":["dataset_id","file_name","source_sha256","node_role","engineering_record_key","indexed_engineering_record","model_locator"],
                    "filter":{"must":[
                      {"key":"dataset_id","match":{"value":ds}}, {"key":"file_name","match":{"value":file}},
                      {"key":"source_sha256","match":{"value":sha}}, {"key":"node_role","match":{"value":"evidence"}},
                      {"key":"model_locator.section","match":{"any":sections}}
                    ],"must_not":[{"is_empty":{"key":"engineering_record_key"}}]}});
                if !cursor.is_null() {
                    request["offset"] = cursor.clone();
                }
                let mut page = store
                    .read_owned(&context, &collection, "scroll", request)
                    .await?;
                let batch = page["points"].as_array_mut().ok_or("INDEX_READ_INVALID")?;
                let nonempty = !batch.is_empty();
                for row in batch.drain(..) {
                    let p = &row["payload"];
                    let loc = &p["model_locator"];
                    let id = indexed_record_id(loc).ok_or(CHANGED)?;
                    let section = loc["section"].as_str().ok_or(CHANGED)?;
                    let key = indexed_record_key(schema, ds, file, sha, section, id);
                    if p["dataset_id"] != ds
                        || p["file_name"] != file
                        || p["source_sha256"] != sha
                        || loc["source_sha256"] != sha
                        || loc["provider"] != "les"
                        || loc["reader"]["format"] != format
                        || !sections.contains(&section)
                        || loc["record_schema"] != schema
                        || p["indexed_engineering_record"]["schema"] != schema
                        || p["engineering_record_key"] != key
                        || !seen.insert(key)
                    {
                        return Err(CHANGED.into());
                    }
                    bytes += serde_json::to_vec(&row)
                        .map_err(|_| "INDEX_READ_INVALID")?
                        .len();
                    if seen.len() > MAX_RECORDS || bytes > MAX_BYTES {
                        return Err(LIMIT.into());
                    }
                    records.push(row);
                }
                let next = page["next_page_offset"].take();
                if next.is_null() {
                    break;
                }
                if !nonempty || next == cursor {
                    return Err("INDEX_READ_INVALID".into());
                }
                cursor = next;
            }
        }
        models.push(ModelRows { doc, records });
    }
    let args = args.clone();
    let pinned = revision.clone();
    let result = if args["mode"] == "dwg_spec" {
        dwg_spec::compare(&context, turn, &collection, &models, &args, pinned).await?
    } else {
        tokio::task::spawn_blocking(move || {
            if args["mode"] == "dwg_tables" {
                dwg_tables::table(models, &args, pinned)
            } else if dwg {
                dwg::table(models, &args, pinned)
            } else {
                rows::table(models, &args, pinned)
            }
        })
        .await
        .map_err(|_| "INDEX_READ_INVALID")??
    };
    if store.revision(&context, &collection).await? != revision {
        return Err(CHANGED.into());
    }
    Ok(result)
}

fn source(doc: &Value, row: &Value) -> Value {
    let mut result = reference(doc, Value::Null);
    result["model_locator"] = row["payload"]["model_locator"].clone();
    result["qdrant_point_id"] = row["id"].clone();
    result["evidence_kind"] = json!("engineering_snapshot");
    result["evidence_admissible"] = json!(true);
    result["reading_representation"] =
        json!(match result["model_locator"]["record_schema"].as_str() {
            Some(DWG_TABLE_ROW_SCHEMA) => "indexed_dwg_table_row",
            Some(DWG_RECORD_SCHEMA) => "indexed_dwg_insert",
            _ => "indexed_rvt_record",
        });
    result
}
