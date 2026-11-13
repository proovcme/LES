//! Registered identities and frozen user scope bind every source read.
mod catalog;
mod parts;
pub(crate) mod retrieval;
mod verification;
mod word;
use super::{envelope, Context, Prepared, Result};
use retrieval::search;
/// Shared data-only scope for chat and MCP; no answer model or dialogue.
pub(crate) struct SourceContext {
    pub request: Value,
    pub scope: Value,
    pub profile: Value,
}
impl From<&Prepared> for SourceContext {
    fn from(turn: &Prepared) -> Self {
        Self {
            request: turn.request.clone(),
            scope: turn.scope.clone(),
            profile: turn.profile.clone(),
        }
    }
}
pub(crate) use verification::physical as verify_physical;
pub(crate) fn selected_files(turn: &Prepared) -> Vec<Value> {
    selected_request(&turn.request)
}
fn selected_scoped(turn: &SourceContext) -> Vec<Value> {
    selected_request(&turn.request)
}
fn selected_request(request: &Value) -> Vec<Value> {
    let mut names = request["target_files"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(name) = request["target_file"].as_str() {
        if !names.contains(&json!(name)) {
            names.push(json!(name));
        }
    }
    names
}
fn bind_selected_name(turn: &SourceContext, args: &mut Value) -> Result<()> {
    bind_name(&selected_scoped(turn), args)
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
pub(super) fn scoped(turn: &SourceContext, dataset: &Value) -> Result<()> {
    if !turn.scope["resolved_dataset_ids"]
        .as_array()
        .is_some_and(|v| v.contains(dataset))
    {
        return Err("TOOL_SCOPE_VIOLATION".into());
    }
    Ok(())
}
pub(crate) fn scoped_document(turn: &SourceContext, doc: &Value) -> Result<()> {
    scoped(turn, &doc["dataset_id"])?;
    let selected = selected_scoped(turn);
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
pub async fn run(context: Arc<Context>, turn: &Prepared, name: &str, args: Value) -> Result<Value> {
    run_scoped(context, &SourceContext::from(turn), name, args).await
}
pub(crate) async fn run_scoped(
    context: Arc<Context>,
    turn: &SourceContext,
    name: &str,
    mut args: Value,
) -> Result<Value> {
    bind_selected_name(turn, &mut args)?;
    if name == "search_sources" {
        return search(context, turn, args).await;
    }
    if name == "dataset_map" {
        return catalog::map(context, turn, args).await;
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
        if source["source_sha256"].as_str().is_none_or(str::is_empty)
            && doc["file_hash"].as_str().is_some_and(|s| s.len() == 64)
        {
            source["source_sha256"] = doc["file_hash"].clone();
        }
        let result=tokio::task::spawn_blocking(move||{
            let request=json!({"kind":"source_page","database":database,"content_root":content_root,"source":source});
            match crate::pdf_text::supervised_source(request.clone()) {
                Err(code) if code=="PDF_QUERY_NOT_FOUND" => {
                    // A missing literal is not a failed physical page read. Read
                    // that same pinned page, explicitly reporting the miss.
                    let mut full=request;
                    full["source"].as_object_mut().ok_or("PDF_SOURCE_INVALID")?.remove("q");
                    let mut value=crate::pdf_text::supervised_source(full)?;
                    value["literal_lookup"]=json!({"matched":false,"code":"PDF_QUERY_NOT_FOUND"});
                    Ok(value)
                },
                result=>result,
            }
        }).await.map_err(|_|"PDF_READ_FAILED")??;
        let mut provenance = doc;
        provenance["file_hash"] = result["source_sha256"].clone();
        let mut source = reference(&provenance, result["page"].clone());
        source["evidence_kind"] = json!("physical_pdf_page");
        source["offset"] = result["offset"].clone();
        source["next_offset"] = result["next_offset"].clone();
        for field in [
            "ocr",
            "ocr_engine",
            "ocr_dpi",
            "ocr_numbers_verified",
            "view_sha256",
            "extractor",
        ] {
            source[field] = result[field].clone();
        }
        source["snippet"] = json!(result["text"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(240)
            .collect::<String>());
        let missing_literal = result["literal_lookup"]["matched"] == false;
        let mut reply = envelope(name, "physical_page", result, vec![source]);
        if missing_literal {
            reply["warnings"]=json!(["Дословное совпадение не найдено. Прочитан текст той же страницы; это не подтверждает наличие искомой фразы. При next_offset продолжи чтение."]);
        }
        return Ok(reply);
    }
    if name == "read_source" {
        if args["view"].as_str().is_some_and(|s| s != "text") {
            return parts::read(context, turn, args).await;
        }
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
