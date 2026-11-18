//! Exact workbook rows from the active artifact for a registered original.
use super::*;

pub(super) async fn read(
    context: Arc<Context>,
    turn: &SourceContext,
    args: &Value,
) -> Result<Option<Value>> {
    let doc = document(&context, args).await?;
    scoped_document(turn, &doc)?;
    let expected = doc["file_hash"]
        .as_str()
        .ok_or("SOURCE_REVISION_REQUIRED")?;
    if args["source_sha256"]
        .as_str()
        .is_some_and(|s| s != expected)
    {
        return Err("SOURCE_REVISION_CHANGED".into());
    }
    let sheet = args["sheet_name"].as_str();
    let first = args["row_start"].as_u64();
    let last = args["row_end"].as_u64().or(first);
    let addressed = first.is_some() || last.is_some();
    if sheet.is_some_and(|s| s.is_empty() || s.chars().count() > 31)
        || (addressed
            && (sheet.is_none()
                || first.is_none()
                || first < Some(1)
                || last < first
                || last > Some(1_048_576)))
    {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(6).clamp(1, 16);
    let expected = expected.to_owned();
    let selection = args.clone();
    let owner = context.clone();
    let original = doc.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Option<Value>> {
        let content = owner.index_store.as_ref().ok_or("INDEX_STORAGE_UNAVAILABLE")?.content_root(&owner);
        let Some((path, sha)) = les_core::tables::artifacts::active(
            &owner.database, &content,
            original["dataset_id"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?,
            original["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?, &expected,
        )? else { return Ok(None) };
        if (offset > 0 || selection["view_sha256"].is_string()) && selection["view_sha256"] != sha {
            return Err("PARQUET_SOURCE_CHANGED".into());
        }
        crate::documents::verified_file(&owner, &original, &expected)?;
        let page = les_core::tables::execute(&json!({
            "action":"select","path":path,"source_sha256":sha,"offset":offset,"limit":limit,
            "sheet_name":selection["sheet_name"],"row_start":selection["row_start"],"row_end":selection["row_end"],
            "q":if addressed {Value::Null} else {selection["q"].clone()}
        }))?;
        let chunks = les_core::tables::execute(&json!({"action":"chunks","rows":page["rows"],
            "dataset_id":original["dataset_id"],"exact_rows":true}))?;
        Ok(Some(json!({"page":page,"chunks":chunks["chunks"]})))
    }).await.map_err(|_|"SOURCE_READ_FAILED")??;
    let Some(result) = result else {
        if sheet.is_some() || addressed {
            return Err("PARQUET_SOURCE_UNAVAILABLE".into());
        }
        return Ok(None);
    };
    let current = document(&context, &json!({"doc_id":doc["id"]})).await?;
    if current["file_hash"] != doc["file_hash"] || current["status"] != doc["status"] {
        return Err("SOURCE_REVISION_CHANGED".into());
    }
    let page = &result["page"];
    let total = page["matches"].as_u64().ok_or("INDEX_READ_INVALID")? as usize;
    if offset > 0 && offset >= total {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let mut used = 0;
    let budget = args["max_chars"]
        .as_u64()
        .or_else(|| args["_context_budget_chars"].as_u64())
        .unwrap_or(3000)
        .clamp(1, 16000) as usize;
    let mut passages = Vec::new();
    let mut sources = Vec::new();
    for chunk in result["chunks"].as_array().ok_or("INDEX_READ_INVALID")? {
        let text = chunk["text"].as_str().ok_or("INDEX_READ_INVALID")?;
        used += text.chars().count();
        if used > budget {
            break;
        }
        let mut source = reference(&doc, Value::Null);
        for key in ["sheet_name", "row"] {
            source[key] = chunk["metadata"][key].clone();
        }
        source["locator"]["sheet"] = source["sheet_name"].clone();
        source["locator"]["row"] = source["row"].clone();
        source["evidence_kind"] = json!("physical_workbook_row");
        source["view_sha256"] = page["source_sha256"].clone();
        source["snippet"] = json!(text.chars().take(240).collect::<String>());
        source["evidence_admissible"] = json!(true);
        sources.push(source.clone());
        passages.push(json!({"text":text,"source":source,
            "canonical":{"schema":"les.document-structure.v1","kind":"paragraph","text":text}}));
    }
    let mut reply = envelope(
        "read_excel_source",
        "workbook_rows",
        json!({"passages":passages,"view_sha256":page["source_sha256"]}),
        sources,
    );
    super::retrieval::page_workbook(
        &mut reply,
        args,
        offset,
        total,
        page["row_count"].as_u64().unwrap_or(0) as usize,
    )?;
    if sheet.is_some() || addressed {
        reply["result"]["coverage"] = json!(if total == 0 { "no_matches" } else { "bounded" });
    }
    if let Some(next) = reply["result"]["next_offset"].as_u64() {
        let mut continuation = json!({"doc_id":doc["id"],"source_sha256":doc["file_hash"],
            "view_sha256":page["source_sha256"],"offset":next});
        for key in ["q", "sheet_name", "row_start", "row_end"] {
            if !args[key].is_null() {
                continuation[key] = args[key].clone();
            }
        }
        reply["result"]["continuation"] = continuation;
    }
    Ok(Some(reply))
}
