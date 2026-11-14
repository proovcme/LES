use super::*;
use crate::chat::tools::sources::{document, scoped_document};
use std::collections::{HashMap, HashSet, VecDeque};
#[derive(Default)]
pub(super) struct Cache(VecDeque<(String, Value)>);
pub(super) async fn snapshot(
    context: Arc<Context>,
    turn: &Prepared,
    selector: Value,
    originals: &mut HashMap<String, Value>,
    cache: &mut Cache,
) -> Result<(Value, Value)> {
    let selected=core(&context.state_root.join("data/document-tasks.sqlite3"),"selector",json!({"selector":selector,"allowed":turn.scope["resolved_dataset_ids"],"files":super::files(turn)})).await?;
    let doc = document(
        &context,
        &json!({"dataset_id":selected["dataset_id"],"file_name":selected["file_name"]}),
    )
    .await?;
    scoped_document(
        &crate::chat::tools::sources::SourceContext::from(turn),
        &doc,
    )?;
    let (config, _, _, _) =
        crate::chat::tools::sources::retrieval::configuration(context.clone(), false).await?;
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let collection = config["collection"]
        .as_str()
        .ok_or("INDEX_COLLECTION_MISMATCH")?;
    let stamp = store.revision(&context, collection).await?;
    let key = json!([selected["dataset_id"], selected["file_name"]]).to_string();
    let source = if let Some(source) = originals.get(&key) {
        source.clone()
    } else {
        let source = core(
            &context.state_root.join("data/document-tasks.sqlite3"),
            "original",
            json!({"document":doc,"content_root":store.content_root(&context)}),
        )
        .await?;
        originals.insert(key, source.clone());
        source
    };
    // Unknown generations never permit cache reuse. The full original hash and
    // exact catalog identity are checked before every hit, including same-stat edits.
    let cache_key = super::digest(&json!([selected, source, stamp]))?;
    if !stamp.is_null() {
        if let Some(index) = cache.0.iter().position(|(key, _)| key == &cache_key) {
            if store.revision(&context, collection).await? != stamp {
                return Err("INDEX_CHANGED_DURING_SEARCH".into());
            }
            let entry = cache.0.remove(index).unwrap();
            let value = entry.1.clone();
            cache.0.push_back(entry);
            return Ok((value, doc));
        }
    }
    let mut must = selected
        .as_object()
        .ok_or("TOOL_SCOPE_VIOLATION")?
        .iter()
        .map(|(k, v)| json!({"key":k,"match":{"value":v}}))
        .collect::<Vec<_>>();
    let filter = json!({"must":std::mem::take(&mut must),"must_not":[{"key":"node_role","match":{"value":"navigation"}},{"key":"evidence_admissible","match":{"value":false}}]});
    let mut points = Vec::new();
    let mut offset = Value::Null;
    let mut seen = HashSet::new();
    let mut chars = 0;
    loop {
        let mut query =
            json!({"filter":filter,"limit":128,"with_payload":true,"with_vector":false});
        if !offset.is_null() {
            query["offset"] = offset.clone();
        }
        let page = store
            .read_owned(&context, collection, "scroll", query)
            .await?;
        let rows = page["points"].as_array().ok_or("INDEX_READ_INVALID")?;
        for row in rows {
            chars += row["payload"]["text"]
                .as_str()
                .unwrap_or("")
                .chars()
                .count();
            if points.len() >= 2048 || chars > 2_000_000 {
                return Err("SOURCE_READ_LIMIT".into());
            }
            if row["payload"]["source_sha256"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && Some(s) != source["sha256"].as_str())
            {
                return Err("INDEX_CHANGED_DURING_SEARCH".into());
            }
            points.push(row.clone());
        }
        offset = page["next_page_offset"].clone();
        if offset.is_null() {
            break;
        }
        if rows.is_empty() || !seen.insert(offset.to_string()) {
            return Err("INDEX_READ_INVALID".into());
        }
    }
    for batch in points.chunks(50) {
        let seeds = batch
            .iter()
            .map(|p| {
                let mut v = p["payload"].clone();
                v["id"] = json!(p["id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| p["id"].to_string()));
                v
            })
            .collect::<Vec<_>>();
        crate::index_store::sections::read(
            &context,
            store,
            &json!({"collection":collection,"revision":stamp,"seeds":seeds,"scopes":[]}),
        )
        .await?;
    }
    if store.revision(&context, collection).await? != stamp {
        return Err("INDEX_CHANGED_DURING_SEARCH".into());
    }
    let snapshot=core(&context.state_root.join("data/document-tasks.sqlite3"),"snapshot",json!({"selector":selected,"allowed":turn.scope["resolved_dataset_ids"],"files":super::files(turn),"points":points,"source":source})).await?;
    if !stamp.is_null() {
        cache.0.push_back((cache_key, snapshot.clone()));
        while cache.0.len() > 8 {
            cache.0.pop_front();
        }
    }
    Ok((snapshot, doc))
}
