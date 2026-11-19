//! Ordinal reads over the existing publication catalog and owned vector storage.
use super::*;

pub(super) fn filter(doc: &Value) -> Value {
    json!({"must":[{"key":"dataset_id","match":{"value":doc["dataset_id"]}},
                  {"key":"file_name","match":{"value":doc["file_name"]}}],
           "must_not":[{"key":"node_role","match":{"value":"navigation"}},
                       {"key":"evidence_admissible","match":{"value":false}}]})
}

pub(super) async fn read(
    context: &Context,
    store: &crate::index_store::Store,
    collection: &str,
    doc: &Value,
    revision: &Value,
    offset: usize,
    limit: usize,
) -> Result<Option<(Vec<Value>, usize)>> {
    if doc["status"] != "INDEXED" || !revision.as_str().is_some_and(|value| !value.is_empty()) {
        return Ok(None);
    }
    let (Some(dataset), Some(file), Some(sha)) = (
        doc["dataset_id"].as_str(),
        doc["file_name"].as_str(),
        doc["file_hash"].as_str().filter(|sha| !sha.is_empty()),
    ) else {
        return Ok(None);
    };
    let database = context.database.clone();
    let scope = [collection, dataset, file].map(str::to_owned);
    let locator = tokio::task::spawn_blocking(move || {
        les_core::index_publication::reading_positions(
            &database, &scope[0], &scope[1], &scope[2], offset, limit,
        )
    })
    .await
    .map_err(|_| "INDEX_READ_UNAVAILABLE")?;
    let Some((total, ids)) = locator else {
        return Ok(None);
    };

    let filter = filter(doc);
    let count = store
        .read_owned(
            context,
            collection,
            "count",
            json!({"filter":filter,"exact":true}),
        )
        .await?;
    if count["count"].as_u64() != Some(total as u64) {
        return Ok(None);
    }

    // The legacy reader collapses PDF points sharing a materialized page.
    // Their fragment positions are a different sequence from reading offsets.
    if file.to_ascii_lowercase().ends_with(".pdf") {
        let mut page_views = filter;
        page_views["must"]
            .as_array_mut()
            .unwrap()
            .push(json!({"key":"pdf_page_view_schema","match":{"value":"les.pdf-page-view.v1"}}));
        let count = store
            .read_owned(
                context,
                collection,
                "count",
                json!({"filter":page_views,"exact":true}),
            )
            .await?;
        if count["count"] != 0 {
            return Ok(None);
        }
    }
    if store.revision(context, collection).await? != *revision {
        return Err("INDEX_CHANGED_DURING_SEARCH".into());
    }
    if offset >= total {
        return Err("SOURCE_OFFSET_INVALID".into());
    }

    let rows = store
        .read_owned(
            context,
            collection,
            "retrieve",
            json!({"ids":ids,"with_payload":true,"with_vector":false}),
        )
        .await?;
    Ok(Some((ordered_rows(rows, &ids, doc, sha, offset)?, total)))
}

fn ordered_rows(
    rows: Value,
    ids: &[String],
    doc: &Value,
    sha: &str,
    offset: usize,
) -> Result<Vec<Value>> {
    let rows = match rows {
        Value::Array(rows) if rows.len() == ids.len() => rows,
        _ => return Err("INDEX_CHANGED_DURING_SEARCH".into()),
    };
    let mut by_id = std::collections::HashMap::new();
    for row in rows {
        let id = row["id"].as_str().ok_or("INDEX_READ_INVALID")?.to_owned();
        if by_id.insert(id, row).is_some() {
            return Err("INDEX_CHANGED_DURING_SEARCH".into());
        }
    }
    ids.iter()
        .enumerate()
        .map(|(position, id)| {
            let row = by_id.remove(id).ok_or("INDEX_CHANGED_DURING_SEARCH")?;
            let p = &row["payload"];
            if p["dataset_id"] != doc["dataset_id"] || p["file_name"] != doc["file_name"] {
                return Err("TOOL_SCOPE_VIOLATION".into());
            }
            if p["source_sha256"] != sha
                || p["chunk_ord"].as_u64() != Some((offset + position) as u64)
                || p["node_role"] == "navigation"
                || p["evidence_admissible"] == false
            {
                return Err("INDEX_CHANGED_DURING_SEARCH".into());
            }
            Ok(row)
        })
        .collect()
}

#[cfg(test)]
#[test]
fn reading_a_page_keeps_position_and_rejects_changed_or_foreign_points() {
    let doc = json!({"dataset_id":"dataset","file_name":"file"});
    let point = |id, position| {
        json!({"id":id,"payload":{"dataset_id":"dataset","file_name":"file",
        "source_sha256":"revision","chunk_ord":position}})
    };
    let ids = vec!["first".into(), "second".into()];
    let first = point("first", 18);
    let second = point("second", 19);
    let points = ordered_rows(json!([second, first]), &ids, &doc, "revision", 18).unwrap();
    assert_eq!(points, vec![first.clone(), second.clone()]);

    let mut changed = second.clone();
    changed["payload"]["source_sha256"] = json!("next-revision");
    assert_eq!(
        ordered_rows(json!([first, changed]), &ids, &doc, "revision", 18)
            .unwrap_err()
            .code,
        "INDEX_CHANGED_DURING_SEARCH"
    );
    changed = second;
    changed["payload"]["dataset_id"] = json!("another-dataset");
    assert_eq!(
        ordered_rows(json!([first, changed]), &ids, &doc, "revision", 18)
            .unwrap_err()
            .code,
        "TOOL_SCOPE_VIOLATION"
    );
}
