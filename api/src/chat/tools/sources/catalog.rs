//! Scoped, paginated filename discovery. Catalog metadata never becomes factual evidence.
use super::*;
pub(super) async fn search(
    context: Arc<Context>,
    turn: &SourceContext,
    args: &Value,
    datasets: &[Value],
) -> Result<Value> {
    let limit = args["limit"].as_u64().unwrap_or(12).clamp(1, 50);
    let result = page(context, turn, args, datasets, limit).await?;
    Ok(envelope(
        "search_sources",
        "document_catalog",
        result,
        vec![],
    ))
}
pub(super) async fn map(context: Arc<Context>, turn: &SourceContext, args: Value) -> Result<Value> {
    let dataset = args["dataset_id"].clone();
    let mut result = page(context, turn, &json!({}), &[dataset.clone()], 100).await?;
    result["dataset_id"] = dataset;
    result["limit"] = json!(100);
    Ok(envelope("dataset_map", "dataset_map", result, vec![]))
}
async fn page(
    context: Arc<Context>,
    turn: &SourceContext,
    args: &Value,
    datasets: &[Value],
    limit: u64,
) -> Result<Value> {
    for dataset in datasets {
        scoped(turn, dataset)?;
    }
    let database = context.database.clone();
    let datasets = datasets.to_vec();
    let q = args["q"].clone();
    let offset = args["offset"].as_u64().unwrap_or(0);
    if offset > 100000 {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let selected = if turn.request["selected_sources_only"] == true {
        selected_scoped(turn)
    } else {
        vec![]
    };
    let result = tokio::task::spawn_blocking(move || -> Result<Value> {
        collect_catalog_page(
            &datasets,
            offset,
            limit,
            20000,
            |dataset, req_offset, req_limit| {
                let input = json!({
                    "dataset_id": dataset,
                    "q": q,
                    "limit": req_limit,
                    "offset": req_offset,
                    "doc_filter": selected
                });
                let page = super::super::super::unwrap(crate::library::catalog(
                    &database,
                    "library_documents",
                    input,
                ))?;
                let count = page["total"].as_u64().ok_or("SOURCE_CATALOG_UNAVAILABLE")?;
                let rows = page["documents"]
                    .as_array()
                    .ok_or("SOURCE_CATALOG_UNAVAILABLE")?
                    .clone();
                Ok((count, rows))
            },
        )
    })
    .await
    .map_err(|_| "SOURCE_CATALOG_UNAVAILABLE")??;
    Ok(result)
}

pub(crate) fn collect_catalog_page<F>(
    datasets: &[Value],
    offset: u64,
    limit: u64,
    max_chars: usize,
    mut fetch: F,
) -> Result<Value>
where
    F: FnMut(&Value, u64, u64) -> Result<(u64, Vec<Value>)>,
{
    let mut documents = Vec::new();
    let mut total = 0u64;
    let mut skip = offset;
    let mut full = false;
    for dataset in datasets {
        if full || documents.len() >= limit as usize {
            let (count, _) = fetch(dataset, 0, 1)?;
            total += count;
            skip = skip.saturating_sub(count);
            continue;
        }
        let (count, _) = fetch(dataset, 0, 1)?;
        total += count;
        if skip >= count {
            skip -= count;
            continue;
        }
        let req_offset = skip;
        let req_limit = limit - documents.len() as u64;
        skip = 0;
        let (_, rows) = fetch(dataset, req_offset, req_limit)?;
        let mut taken_in_dataset = 0u64;
        for doc in rows {
            documents.push(doc);
            if documents.len() > 1
                && serde_json::to_string(&documents)
                    .map(|s| s.chars().count())
                    .unwrap_or(0)
                    > max_chars
            {
                documents.pop();
                full = true;
                break;
            }
            taken_in_dataset += 1;
        }
        if req_offset + taken_in_dataset < count {
            full = true;
        }
    }
    let next = offset + documents.len() as u64;
    Ok(json!({
        "documents": documents,
        "total": total,
        "offset": offset,
        "next_offset": if next < total && !documents.is_empty() { Some(next) } else { None },
        "evidence_admissible": false,
        "note": "Это имена и статусы файлов, а не их содержимое. Найденный документ ещё нужно прочитать. Дата файла и порядок выдачи не определяют актуальность редакции."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_pagination_preserves_contiguous_order_across_datasets_when_char_budget_hits() {
        let ds_a: Vec<Value> = (0..30)
            .map(|i| {
                json!({
                    "id": format!("A{i}"),
                    "dataset_id": "ds-a",
                    "file_name": format!("{}A{i:02}.pdf", "Раздел_А_Длинный_Путь/".repeat(25)),
                    "status": "INDEXED"
                })
            })
            .collect();
        let ds_b: Vec<Value> = (0..30)
            .map(|i| {
                json!({
                    "id": format!("B{i}"),
                    "dataset_id": "ds-b",
                    "file_name": format!("B{i:02}.pdf"),
                    "status": "INDEXED"
                })
            })
            .collect();
        let datasets = vec![json!("ds-a"), json!("ds-b")];
        let mut cursor = Some(0u64);
        let mut visited = Vec::new();
        while let Some(offset) = cursor {
            let page = collect_catalog_page(&datasets, offset, 100, 20000, |ds, off, lim| {
                let rows = if ds == "ds-a" { &ds_a } else { &ds_b };
                let slice = rows
                    .iter()
                    .skip(off as usize)
                    .take(lim as usize)
                    .cloned()
                    .collect();
                Ok((rows.len() as u64, slice))
            })
            .unwrap();
            assert_eq!(page["total"], 60);
            assert_eq!(page["offset"], offset);
            let docs = page["documents"].as_array().unwrap();
            assert!(!docs.is_empty());
            assert!(serde_json::to_string(docs).unwrap().chars().count() <= 20000);
            for d in docs {
                visited.push(d["id"].as_str().unwrap().to_owned());
            }
            cursor = page["next_offset"].as_u64();
        }
        let expected: Vec<String> = (0..30)
            .map(|i| format!("A{i}"))
            .chain((0..30).map(|i| format!("B{i}")))
            .collect();
        assert_eq!(visited, expected);
    }
}
