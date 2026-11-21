//! Exact workbook rows from the active artifact for a registered original.
use super::*;
use std::collections::{BTreeMap, HashMap};

pub(super) fn addressed_range(query: &str, args: &mut Value) {
    let coordinates = les_core::chat_request::sheet_rows(query);
    if let Some((sheet, rows)) = coordinates
        .first_key_value()
        .filter(|_| coordinates.len() == 1)
    {
        if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
            if (*last - *first + 1) == rows.len() as u64 {
                args["sheet_name"] = json!(sheet);
                args["row_start"] = json!(first);
                args["row_end"] = json!(last);
                args["limit"] = json!(rows.len().clamp(1, 16));
            }
        }
    }
}

pub(super) async fn group_evidence(
    context: Arc<Context>,
    turn: &SourceContext,
    query: &str,
    candidates: &[Value],
    max_chars: usize,
) -> Value {
    let mut passages = Vec::new();
    let mut sources = Vec::new();
    if les_core::tables::group_identifiers(query).next().is_none() {
        return Value::Null;
    }
    let mut used = 0;
    for candidate in candidates
        .iter()
        .filter(|book| {
            book["file_name"]
                .as_str()
                .is_some_and(|file| super::retrieval::original_reader(file) == "read_excel_source")
        })
        .take(8)
    {
        if used >= max_chars {
            break;
        }
        let args = json!({"doc_id":candidate["doc_id"],"source_sha256":candidate["source_sha256"],
            "q":query,"limit":4,"max_chars":max_chars-used,"_match_group":true});
        match read(context.clone(), turn, &args).await {
            Ok(Some(reply)) => {
                for row in reply["result"]["passages"].as_array().into_iter().flatten() {
                    used += row["text"].as_str().unwrap_or("").chars().count();
                    sources.push(row["source"].clone());
                    passages.push(row.clone());
                }
            }
            Ok(None) => (),
            Err(error) => eprintln!(
                "{}",
                json!({"event":"workbook_group_read_unavailable","code":error.code})
            ),
        }
    }
    if passages.is_empty() {
        return Value::Null;
    }
    envelope(
        "search_sources",
        "workbook_group_rows",
        json!({"passages":passages,"coverage":"bounded","pull_search":true}),
        sources,
    )
}

pub(super) async fn attach_groups(
    context: &Context,
    passages: &mut [Value],
    sources: &mut [Value],
) {
    let Some(store) = &context.index_store else {
        return;
    };
    let mut books = BTreeMap::<_, Vec<_>>::new();
    for (index, source) in sources.iter().enumerate() {
        let (Some(dataset), Some(file), Some(revision), Some(sheet), Some(row)) = (
            source["dataset_id"].as_str(),
            source["doc_name"].as_str(),
            source["source_sha256"].as_str(),
            source["sheet_name"].as_str(),
            source["row"].as_u64(),
        ) else {
            continue;
        };
        if super::retrieval::original_reader(file) == "read_excel_source"
            && source["evidence_admissible"] != false
        {
            books
                .entry([dataset, file, revision].map(str::to_owned))
                .or_default()
                .push((index, sheet.to_owned(), row));
        }
    }
    if books.is_empty() {
        return;
    }
    let database = context.database.clone();
    let content = store.content_root(context);
    let found = tokio::task::spawn_blocking(move || {
        let mut found = Vec::new();
        for ([dataset, file, revision], rows) in books {
            let result = les_core::tables::artifacts::active(
                &database, &content, &dataset, &file, &revision,
            )
            .and_then(|artifact| {
                let Some((path, sha)) = artifact else {
                    return Ok(Value::Null);
                };
                let targets = rows
                    .iter()
                    .map(|(_, sheet, row)| json!({"sheet_name":sheet,"row":row}))
                    .collect::<Vec<_>>();
                les_core::tables::execute(
                    &json!({"action":"groups","path":path,"source_sha256":sha,"rows":targets}),
                )
            });
            let data = match result {
                Ok(data) => data,
                Err(code) => {
                    eprintln!(
                        "{}",
                        json!({"event":"workbook_groups_unavailable","code":code})
                    );
                    continue;
                }
            };
            let groups: HashMap<_, _> = data["rows"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    Some((
                        (item["sheet_name"].as_str()?, item["row"].as_u64()?),
                        &item["workbook_group"],
                    ))
                })
                .collect();
            for (index, sheet, row) in rows {
                if let Some(group) = groups.get(&(sheet.as_str(), row)) {
                    found.push((index, (*group).clone()));
                }
            }
        }
        found
    })
    .await;
    match found {
        Ok(found) => {
            for (index, group) in found {
                sources[index]["workbook_group"] = group.clone();
                passages[index]["source"]["workbook_group"] = group;
            }
        }
        Err(_) => eprintln!(
            "{}",
            json!({"event":"workbook_groups_unavailable","code":"SOURCE_READ_FAILED"})
        ),
    }
}

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
        let page = les_core::tables::execute(&json!({
            "action":"select","path":path,"source_sha256":sha,"offset":offset,"limit":limit,
            "sheet_name":selection["sheet_name"],"row_start":selection["row_start"],"row_end":selection["row_end"],
            "match_group":selection["_match_group"],
            "q":if addressed {Value::Null} else {selection["q"].clone()}
        }))?;
        if selection["_match_group"] != true || page["matches"].as_u64().unwrap_or(0) > 0 {
            crate::documents::verified_file(&owner, &original, &expected)?;
        }
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
        if let Some(group) = chunk["metadata"].get("workbook_group") {
            source["workbook_group"] = group.clone();
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
        for key in [
            "q",
            "sheet_name",
            "row_start",
            "row_end",
            "limit",
            "max_chars",
        ] {
            if !args[key].is_null() {
                continuation[key] = args[key].clone();
            }
        }
        reply["result"]["continuation"] = continuation;
    }
    Ok(Some(reply))
}
