//! Registered identities and frozen user scope bind every source read.
pub(crate) mod retrieval;
mod verification;
use super::{envelope, Context, Prepared, Result};
use retrieval::search;
pub(crate) use verification::physical as verify_physical;
pub(crate) fn selected_files(turn: &Prepared) -> Vec<Value> {
    let mut names = turn.request["target_files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(name) = turn.request["target_file"].as_str() {
        if !names.contains(&json!(name)) {
            names.push(json!(name));
        }
    }
    names
}
fn bind_selected_name(turn: &Prepared, args: &mut Value) -> Result<()> {
    bind_name(&selected_files(turn), args)
}
fn bind_name(selected: &[Value], args: &mut Value) -> Result<()> {
    if let Some(name) = args["doc_name"].as_str() {
        // An explicit path remains explicit. Only a short name can be resolved
        // through the user's frozen selection; another path is never rewritten.
        if name.contains(['/', '\\']) {
            return Ok(());
        }
        let names: Vec<_> = selected
            .iter()
            .filter(|v| {
                v.as_str().is_some_and(|s| {
                    s.rsplit(['/', '\\']).next() == name.rsplit(['/', '\\']).next()
                })
            })
            .collect();
        if names.len() == 1 {
            args["doc_name"] = names[0].clone();
        } else if names.len() > 1 && !selected.contains(&json!(name)) {
            return Err("SOURCE_IDENTITY_AMBIGUOUS".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_unique_short_name_is_bound_to_the_explicit_selection() {
        let selected = vec![json!("folder/Source.pdf")];
        let mut args = json!({"doc_name":"Source.pdf"});
        bind_name(&selected, &mut args).unwrap();
        assert_eq!(args["doc_name"], "folder/Source.pdf");
        args["doc_name"] = json!("different/Source.pdf");
        bind_name(&selected, &mut args).unwrap();
        assert_eq!(args["doc_name"], "different/Source.pdf");
        args["doc_name"] = json!("Source.pdf");
        assert!(bind_name(&[json!("a/Source.pdf"), json!("b/Source.pdf")], &mut args).is_err());
    }
}
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
pub(crate) async fn document(context: &Context, args: &Value) -> Result<Value> {
    let database = context.database.clone();
    let mut args = args.clone();
    if let Some(name) = args.get("doc_name").cloned() {
        args["file_name"] = name;
    }
    let doc = tokio::task::spawn_blocking(move || {
        super::super::unwrap(crate::library::catalog(&database, "library_document", args))
    })
    .await
    .map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
    if doc.is_null() {
        return Err("SOURCE_DOCUMENT_NOT_FOUND".into());
    }
    Ok(doc)
}
pub(super) fn scoped(turn: &Prepared, dataset: &Value) -> Result<()> {
    if !turn.scope["resolved_dataset_ids"]
        .as_array()
        .is_some_and(|v| v.contains(dataset))
    {
        return Err("TOOL_SCOPE_VIOLATION".into());
    }
    Ok(())
}
pub(crate) fn scoped_document(turn: &Prepared, doc: &Value) -> Result<()> {
    scoped(turn, &doc["dataset_id"])?;
    let selected = selected_files(turn);
    if turn.request["selected_sources_only"] == true
        && !selected.is_empty()
        && !selected.contains(&doc["file_name"])
    {
        return Err("TOOL_SCOPE_VIOLATION".into());
    }
    if doc["status"] == "MISSING" {
        return Err("SOURCE_DOCUMENT_MISSING".into());
    }
    Ok(())
}
pub(crate) fn reference(doc: &Value, page: Value) -> Value {
    let page = page
        .as_u64()
        .or_else(|| page.as_str()?.parse().ok())
        .filter(|p| *p > 0)
        .map(|p| json!(p))
        .unwrap_or(Value::Null);
    let mut uri = reqwest::Url::parse("http://localhost/api/documents/by-id/").unwrap();
    uri.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(doc["id"].as_str().unwrap_or(""))
        .push("raw");
    uri.query_pairs_mut()
        .append_pair("source_sha256", doc["file_hash"].as_str().unwrap_or(""));
    json!({"doc_id":doc["id"],"dataset_id":doc["dataset_id"],"doc_name":doc["file_name"],
        "source_sha256":doc["file_hash"],"page":page,"source_page":page,
        "locator":{"source_sha256":doc["file_hash"],"source_page":page},
        "url":format!("{}?{}{}",uri.path(),uri.query().unwrap_or(""),page.as_u64().map(|p|format!("#page={p}")).unwrap_or_default())})
}
pub async fn run(
    context: Arc<Context>,
    turn: &Prepared,
    name: &str,
    mut args: Value,
) -> Result<Value> {
    bind_selected_name(turn, &mut args)?;
    if name == "search_sources" {
        return search(context, turn, args).await;
    }
    if name == "dataset_map" {
        scoped(turn, &args["dataset_id"])?;
        let db = context.database.clone();
        let dataset = args["dataset_id"].clone();
        let result = tokio::task::spawn_blocking(move || {
            super::super::unwrap(crate::library::catalog(
                &db,
                "library_documents",
                json!({"dataset_id":dataset,"limit":100,"offset":0}),
            ))
        })
        .await
        .map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
        return Ok(envelope(name, "dataset_map", result, vec![]));
    }
    if name == "read_pdf_source" {
        let doc = document(&context, &args).await?;
        scoped_document(turn, &doc)?;
        // Original uploads stay in the application-owned source area even when
        // immutable indexed derivatives have moved into Edge/content.
        let database = context.database.clone();
        let content_root = context.state_root.join("storage/datasets");
        let mut source = args;
        source["doc_id"] = doc["id"].clone();
        source["dataset_id"] = doc["dataset_id"].clone();
        source["doc_name"] = doc["file_name"].clone();
        let result=tokio::task::spawn_blocking(move||{
            crate::pdf_text::supervised_source(json!({"kind":"source_page","database":database,"content_root":content_root,"source":source}))
        }).await.map_err(|_|"PDF_READ_FAILED")??;
        let mut provenance = doc;
        provenance["file_hash"] = result["source_sha256"].clone();
        let mut source = reference(&provenance, result["page"].clone());
        source["evidence_kind"] = json!("physical_pdf_page");
        source["offset"] = result["offset"].clone();
        source["next_offset"] = result["next_offset"].clone();
        source["snippet"] = json!(result["text"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(240)
            .collect::<String>());
        return Ok(envelope(name, "physical_page", result, vec![source]));
    }
    if name == "read_source" {
        return retrieval::read(context, turn, args).await;
    }
    if name == "read_excel_source" {
        let mut result = retrieval::read(context, turn, args).await?;
        result["tool"] = json!(name);
        result["warnings"]=json!(["Прочитаны проверенные индексированные разделы книги; это не полная выгрузка всех ячеек."]);
        return Ok(result);
    }
    Err("NATIVE_SOURCE_CAPABILITY_PENDING".into())
}
