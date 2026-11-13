//! Bounded promotion from a search hit to a complete indexed document.
//! A large document stays on the existing section reader; it is never scrolled
//! to exhaustion just to discover whether it fits the model window.
use super::*;

pub(super) async fn read(
    context: &Context,
    store: &Store,
    collection: &str,
    seeds: &[Value],
    budget: usize,
) -> Result<Vec<Value>, &'static str> {
    let mut seen = HashSet::new();
    let mut documents = Vec::new();
    for seed in seeds {
        let key = [seed["dataset_id"].clone(), seed["file_name"].clone()];
        if !seen.insert(key.clone()) {
            continue;
        }
        let filter = json!({"must":[{"key":"dataset_id","match":{"value":key[0]}},
            {"key":"file_name","match":{"value":key[1]}}],
            "must_not":[{"key":"node_role","match":{"value":"navigation"}},
                {"key":"evidence_admissible","match":{"value":false}}]});
        // One bounded probe, including an extra row to detect the size limit.
        let page = request(
            context,
            store,
            collection,
            true,
            json!({"filter":filter,"limit":33,
            "with_payload":true,"with_vector":false}),
        )
        .await?;
        let rows = page["points"].as_array().ok_or(UNAVAILABLE)?;
        if rows.len() > 33 {
            return Err(LIMIT);
        }
        let mut points = Vec::new();
        let mut ids = HashSet::new();
        let mut chars = 0;
        for row in rows {
            let payload = &row["payload"];
            if payload["dataset_id"] != key[0]
                || payload["file_name"] != key[1]
                || !evidence(payload)
            {
                return Err(SCOPE);
            }
            if seeds
                .iter()
                .filter(|s| s["dataset_id"] == key[0] && s["file_name"] == key[1])
                .any(|s| {
                    s["source_sha256"].as_str().is_some_and(|h| !h.is_empty())
                        && s["source_sha256"] != payload["source_sha256"]
                })
            {
                return Err(CHANGED);
            }
            let id = id(&row["id"])?;
            if !ids.insert(id.clone()) {
                return Err(CHANGED);
            }
            chars += payload["text"].as_str().unwrap_or_default().chars().count();
            points.push(json!({"id":id,"payload":payload}));
        }
        if !page["next_page_offset"].is_null() || points.len() > 32 || chars > budget {
            continue;
        }
        if !seeds
            .iter()
            .filter(|s| s["dataset_id"] == key[0] && s["file_name"] == key[1])
            .all(|s| ids.contains(s["id"].as_str().unwrap_or("")))
        {
            return Err(CHANGED);
        }
        points.sort_by_key(|p| p["payload"]["chunk_ord"].as_u64().unwrap_or(0));
        documents.push(json!({"scope":{"dataset_id":key[0],"file_name":key[1]},"points":points}));
    }
    Ok(documents)
}
