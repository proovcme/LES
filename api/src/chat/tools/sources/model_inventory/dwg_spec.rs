//! Read one pinned workbook and the saved drawing records in the same dataset.
mod matching;

use super::{
    document, envelope, reference, scoped_document, selected_scoped, source, ModelRows, Result,
    SourceContext, CHANGED, LIMIT, TOOL,
};
use crate::chat::{unwrap, Context};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};

const NO_SPEC: &str = "DWG_SPEC_WORKBOOK_REQUIRED";
const BOOK_LIMIT: u64 = 10_000;

pub(super) fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|v| !v.is_empty())
}

fn workbook(doc: &Value) -> bool {
    text(&doc["file_name"])
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, ext)| {
            matches!(ext.to_ascii_lowercase().as_str(), "xlsx" | "xlsm" | "xls")
        })
}

async fn choose_book(
    context: &Context,
    turn: &SourceContext,
    dataset: &Value,
    args: &Value,
) -> Result<Value> {
    let names = selected_scoped(turn);
    let chosen = if args["spec_doc_id"].is_string() {
        document(context, &json!({"doc_id":args["spec_doc_id"]})).await?
    } else {
        let db = context.database.clone();
        let mut offset = 0;
        let dataset = dataset.clone();
        let names = names.clone();
        let ids = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut ids = Vec::new();
            loop {
                let result = unwrap(crate::library::catalog(&db, "library_documents", json!({
                    "dataset_id":dataset,"q":".xls","doc_filter":names,"limit":100,"offset":offset
                })))?;
                let rows = result["documents"].as_array().ok_or("SOURCE_CATALOG_UNAVAILABLE")?;
                ids.extend(rows.iter().filter(|doc| workbook(doc)).map(|doc| doc["id"].clone()));
                if ids.len() > 1 {
                    return Err(NO_SPEC.into());
                }
                offset += rows.len();
                if rows.is_empty() || offset as u64 >= result["total"].as_u64().unwrap_or(0) {
                    return Ok(ids);
                }
            }
        }).await.map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
        document(context, &json!({"doc_id":ids.first().ok_or(NO_SPEC)?})).await?
    };
    scoped_document(turn, &chosen)?;
    let ids = turn.request["scope"]["document_ids"].as_array();
    if &chosen["dataset_id"] != dataset
        || (!names.is_empty() && !names.contains(&chosen["file_name"]))
        || ids.is_some_and(|ids| !ids.is_empty() && !ids.contains(&chosen["id"]))
    {
        return Err("TOOL_SCOPE_VIOLATION".into());
    }
    if !workbook(&chosen) || chosen["status"] != "INDEXED" {
        return Err(NO_SPEC.into());
    }
    Ok(chosen)
}

fn owner(model: &ModelRows) -> Result<Option<&str>> {
    let mut owners = BTreeSet::new();
    for row in &model.records {
        let record = &row["payload"]["indexed_engineering_record"];
        if record["space"] != "model" {
            return Err("INDEX_READ_INVALID".into());
        }
        owners.insert(text(&record["owner_handle"]).ok_or("INDEX_READ_INVALID")?);
    }
    if owners.len() > 1 {
        return Err("INDEX_READ_INVALID".into());
    }
    Ok(owners.first().copied())
}

fn note_text<'a>(card: &'a str, handle: &str, owner: &str) -> Option<&'a str> {
    let kind = format!("\nID: {handle}; раздел: elements; категория: ");
    let (_, fields) = card.split_once(&kind)?;
    let (kind, fields) = fields.split_once('\n')?;
    if !matches!(kind, "MTEXT" | "TEXT") {
        return None;
    }
    let (_, stored) =
        fields.split_once(&format!("\nproperties / ownerBlockRecordSoftId: {owner}\n"))?;
    let caption = stored
        .strip_prefix("properties / text: ")?
        .strip_suffix("\nvalue_semantics: saved_dwg")?;
    (!caption.is_empty() && caption.len() <= 4096).then_some(caption)
}

async fn notes(
    context: &Context,
    collection: &str,
    models: &[&ModelRows],
) -> Result<Vec<matching::Note>> {
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let mut notes = Vec::new();
    let mut visited = HashSet::new();
    let mut ids_read = 0;
    for model in models {
        let Some(owner) = owner(model)? else { continue };
        let doc = &model.doc;
        let collection_name = collection.to_owned();
        let db = context.database.clone();
        let dataset = text(&doc["dataset_id"]).ok_or(CHANGED)?.to_owned();
        let name = text(&doc["file_name"]).ok_or(CHANGED)?.to_owned();
        let model_owner = owner.to_owned();
        let ids = tokio::task::spawn_blocking(move || {
            les_core::index_publication::dwg_note_points(
                &db,
                &collection_name,
                &dataset,
                &name,
                &model_owner,
            )
        })
        .await
        .map_err(|_| "INDEX_READ_INVALID")??;
        ids_read += ids.len();
        if ids_read > super::MAX_RECORDS {
            return Err(LIMIT.into());
        }
        for ids in ids.chunks(80) {
            let payload = store.read_owned(context, collection, "retrieve", json!({
                "ids":ids,"with_vectors":false,
                "with_payload":["dataset_id","file_name","source_sha256","node_role","model_locator","text"]
            })).await?;
            let rows = payload.as_array().ok_or("INDEX_READ_INVALID")?;
            if rows.len() != ids.len() {
                return Err(CHANGED.into());
            }
            for row in rows {
                let p = &row["payload"];
                let loc = &p["model_locator"];
                if !text(&row["id"]).is_some_and(|id| ids.iter().any(|expected| id == expected))
                    || p["dataset_id"] != doc["dataset_id"]
                    || p["file_name"] != doc["file_name"]
                    || p["source_sha256"] != doc["file_hash"]
                    || loc["source_sha256"] != doc["file_hash"]
                    || p["node_role"] != "evidence"
                    || loc["provider"] != "les"
                    || loc["reader"]["format"] != "dwg"
                    || loc["section"] != "elements"
                {
                    return Err(CHANGED.into());
                }
                let handle = text(&loc["native_id"]).ok_or(CHANGED)?;
                let Some(value) = text(&p["text"]).and_then(|card| note_text(card, handle, owner))
                else {
                    return Err(CHANGED.into());
                };
                if visited.insert((text(&doc["id"]).ok_or(CHANGED)?, handle.to_owned())) {
                    let mut evidence = source(doc, row);
                    evidence["reading_representation"] = json!("indexed_dwg_annotation");
                    evidence["snippet"] = json!(value.chars().take(240).collect::<String>());
                    notes.push(matching::Note {
                        raw: value.to_owned(),
                        source: evidence,
                    });
                }
            }
        }
    }
    Ok(notes)
}

pub(super) async fn compare(
    context: &Context,
    turn: &SourceContext,
    collection: &str,
    models: &[ModelRows],
    args: &Value,
    revision: Value,
) -> Result<Value> {
    let dataset = &models[0].doc["dataset_id"];
    if models.iter().any(|m| &m.doc["dataset_id"] != dataset) {
        return Err("DWG_SPEC_SINGLE_DATASET".into());
    }
    let book = choose_book(context, turn, dataset, args).await?;
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let content = store.content_root(context);
    let db = context.database.clone();
    let selected = book.clone();
    let data = tokio::task::spawn_blocking(move || -> Result<Value> {
        let (path, sha) = les_core::tables::artifacts::active(
            &db,
            &content,
            text(&selected["dataset_id"]).ok_or(CHANGED)?,
            text(&selected["file_name"]).ok_or(CHANGED)?,
            text(&selected["file_hash"]).ok_or(CHANGED)?,
        )?
        .ok_or("PARQUET_SOURCE_UNAVAILABLE")?;
        let data = les_core::tables::execute(&json!({"action":"read","path":path,
            "source_sha256":sha,"limit":BOOK_LIMIT}))?;
        if data["complete"] != true {
            return Err(LIMIT.into());
        }
        Ok(data)
    })
    .await
    .map_err(|_| "SOURCE_READ_FAILED")??;
    let rows = data["rows"].as_array().ok_or("INDEX_READ_INVALID")?;
    let positions = matching::positions(rows, args)?;
    let mut hashes = BTreeSet::new();
    let mut checked = Vec::new();
    let mut coverage = Vec::new();
    for model in models {
        let owned = owner(model)?.is_some();
        let status = if !owned {
            "model_owner_unavailable"
        } else if hashes.insert(text(&model.doc["file_hash"]).ok_or(CHANGED)?) {
            checked.push(model);
            "read"
        } else {
            "duplicate_revision"
        };
        coverage.push(
            json!({"doc_id":model.doc["id"],"doc_name":model.doc["file_name"],
            "source_sha256":model.doc["file_hash"],"status":status}),
        );
    }
    if checked.is_empty() {
        return Err("DWG_SPEC_RECORDS_REQUIRED".into());
    }
    let captions = notes(context, collection, &checked).await?;
    let current = document(context, &json!({"doc_id":book["id"]})).await?;
    if current["file_hash"] != book["file_hash"] || current["status"] != book["status"] {
        return Err(CHANGED.into());
    }
    let (mut table, sources) = matching::table(&book, positions, &checked, captions, args)?;
    table["index_revision"] = revision;
    table["models"] = json!(coverage);
    Ok(envelope(TOOL, "dwg_spec", table, sources))
}
