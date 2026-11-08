//! Query-time dense/BM25/RRF followed by generation-checked parent reads.
use super::*;
use crate::index_batch::configuration::{identity, tokenizer, Settings};
fn query_text(settings: &Settings, query: &str) -> Result<String> {
    match settings.get("RAG_QUERY_EMBEDDING_MODE","raw-v1") {
        "raw-v1"=>Ok(query.into()),
        // The same explicit asymmetric contract as the legacy adapter. It changes
        // only query embeddings; indexed document vectors are never rewritten.
        "qwen-retrieval-v1" if settings.get("LES_EMBED_PROFILE","legacy")=="qwen"=>Ok(format!("Instruct: Given a search query, retrieve relevant passages from the selected corpus\nQuery: {query}")),
        _=>Err("QUERY_EMBEDDING_CONTRACT_INVALID".into())
    }
}
async fn configuration(
    context: Arc<Context>,
    embedding: bool,
) -> Result<(Value, Value, String, bool)> {
    tokio::task::spawn_blocking(move||->Result<_>{
        let settings=Settings::load(&context.state_root)?;
        let (_,path)=identity(&context,&settings)?;
        let bytes=std::fs::read(path).map_err(|_|"INDEX_NOT_CONFIGURED")?;
        if bytes.len()>1024*1024{return Err("INDEX_SETTINGS_INVALID".into());}
        let contract:Value=serde_json::from_slice(&bytes).map_err(|_|"INDEX_SETTINGS_INVALID")?;
        let frozen=les_core::index_configuration::execute(&json!({"contract":contract,"settings":settings.core_values(),"tokenizer":tokenizer(&settings,&contract)?}))?;
        if !embedding {return Ok((frozen,Value::Null,String::new(),false));}
        let execution=super::super::super::unwrap(crate::library::catalog(&context.database,"model_execution_snapshot",json!({"role":"embeddings"})))?;
        if execution.is_null(){return Err("EMBEDDING_MODEL_NOT_ASSIGNED".into());}
        let env=std::env::var_os("LES_ENV_PATH").map(PathBuf::from).unwrap_or_else(||context.state_root.join(".env"));
        let binding=crate::model_runtime::resolve(&execution,"embeddings",&env)?;
        if les_core::index_bootstrap::model_key(binding.revision["model_id"].as_str().unwrap_or(""))!=les_core::index_bootstrap::model_key(contract["embedding_model"].as_str().unwrap_or("")){
            return Err("EMBEDDING_INDEX_MODEL_MISMATCH".into());
        }
        let consent=["true","1","yes","on"].contains(&settings.get("LES_CLOUD_CONSENT","false").to_ascii_lowercase().as_str());
        if binding.endpoint.locality=="remote"&&!consent{return Err("CLOUD_CONSENT_REQUIRED".into());}
        let revision=binding.revision["revision_id"].clone();
        // Keep the query transform in this frozen request rather than global state.
        let prefix=query_text(&settings,"")?;
        Ok((frozen,revision,prefix,consent))
    }).await.map_err(|_|"INDEX_SETTINGS_INVALID")?
}
pub(super) async fn search(context: Arc<Context>, turn: &Prepared, args: Value) -> Result<Value> {
    let requested = args["dataset_ids"].as_array().cloned().unwrap_or_else(|| {
        turn.scope["resolved_dataset_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    });
    let datasets = if let Some(id) = args["dataset_id"].as_str().filter(|s| !s.is_empty()) {
        vec![json!(id)]
    } else {
        requested
    };
    if datasets.is_empty() {
        return Err("CHAT_SCOPE_EMPTY".into());
    }
    for dataset in &datasets {
        scoped(turn, dataset)?;
    }
    if turn.request["reranker_enabled"] == true {
        return Err("NATIVE_RERANKER_CAPABILITY_PENDING".into());
    }
    let query = args["q"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 20000)
        .ok_or("INDEX_QUERY_INVALID")?;
    let (config, revision, prefix, allow_remote) = configuration(context.clone(), true).await?;
    let raw=crate::embedding_inference::execute(context.clone(),&json!({"schema":"les.embedding-inference.v1","revision_id":revision,"inputs":[format!("{prefix}{query}")],
        "dimension":config["embedding"]["vector_size"].as_str().and_then(|s|s.parse::<u64>().ok()),"timeout_seconds":120,"allow_remote":allow_remote,"response_limit":1048576,"release_after":true})).await?;
    let end = raw
        .iter()
        .position(|c| *c == b'\n')
        .ok_or("UPSTREAM_EMBEDDING_RESPONSE_INVALID")?;
    let header: Value =
        serde_json::from_slice(&raw[..end]).map_err(|_| "UPSTREAM_EMBEDDING_RESPONSE_INVALID")?;
    let dimension = header["dimension"]
        .as_u64()
        .filter(|n| *n <= 65536)
        .ok_or("UPSTREAM_EMBEDDING_RESPONSE_INVALID")? as usize;
    let expected = config["embedding"]["vector_size"]
        .as_str()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or("INDEX_CONTRACT_MISMATCH")?;
    if header["count"] != 1 || dimension != expected || raw.len() - end - 1 != dimension * 8 {
        return Err("EMBEDDING_DIMENSION_MISMATCH".into());
    }
    let vector: Vec<_> = raw[end + 1..]
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
        .collect();
    if vector.iter().any(|v| !v.is_finite()) {
        return Err("UPSTREAM_EMBEDDING_RESPONSE_INVALID".into());
    }
    let files = if args["doc_id"].as_str().is_some_and(|s| !s.is_empty()) {
        let doc = document(&context, &args).await?;
        scoped_document(turn, &doc)?;
        vec![doc["file_name"].clone()]
    } else if let Some(name) = args["doc_name"].as_str().filter(|s| !s.is_empty()) {
        vec![json!(name)]
    } else {
        selected_files(turn)
    };
    if turn.request["selected_sources_only"] == true {
        let selected = selected_files(turn);
        if !selected.is_empty() && files.iter().any(|v| !selected.contains(v)) {
            return Err("TOOL_SCOPE_VIOLATION".into());
        }
    }
    let policy = les_core::chat_profiles::retrieval_policy(&turn.profile);
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(policy["model_evidence_k"].as_u64().unwrap_or(6))
        .clamp(1, 16) as usize;
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let result=store.hybrid_owned(&context,json!({"collection":config["collection"],"text":query,"vector":vector,"dense_name":config["vector_names"]["dense"],
        "limit":policy["retrieval_candidate_k"],"rank_constant":2,"filters":{"dataset_ids":datasets,"doc_filter":files},"hierarchy":true})).await?;
    let points = result["points"]
        .as_array()
        .ok_or("INDEX_READ_INVALID")?
        .iter()
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    passages(
        &context,
        turn,
        &config["collection"],
        points,
        result["revision"].clone(),
        args["max_chars"].as_u64().unwrap_or(3000).clamp(1, 16000) as usize,
        "search_sources",
    )
    .await
}
async fn passages(
    context: &Context,
    turn: &Prepared,
    collection: &Value,
    points: Vec<Value>,
    revision: Value,
    max_chars: usize,
    name: &str,
) -> Result<Value> {
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let mut seeds = Vec::new();
    let mut scopes = Vec::new();
    for point in &points {
        let mut seed = point["payload"].clone();
        scoped(turn, &seed["dataset_id"])?;
        seed["id"] = json!(point["id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| point["id"].to_string()));
        let scope = json!({"dataset_id":seed["dataset_id"],"file_name":seed["file_name"],"parent_id":seed["parent_id"]});
        if seed["parent_id"].as_str().is_some_and(|v| !v.is_empty())
            && !scopes.contains(&scope)
            && scopes.len() < 8
        {
            scopes.push(scope);
        }
        seeds.push(seed);
    }
    if points.is_empty() {
        return Ok(envelope(
            name,
            "hybrid_search",
            json!({"passages":[],"coverage":"no_matches","index_revision":revision}),
            vec![],
        ));
    }
    let hydrated = crate::index_store::sections::read(
        context,
        store,
        &json!({"collection":collection,"revision":revision,"seeds":seeds,"scopes":scopes}),
    )
    .await?;
    let mut selected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for point in &points {
        let payload = &point["payload"];
        if let Some(section) = hydrated["sections"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| {
                s["scope"]["dataset_id"] == payload["dataset_id"]
                    && s["scope"]["file_name"] == payload["file_name"]
                    && s["scope"]["parent_id"] == payload["parent_id"]
            })
        {
            let mut rows = section["points"].as_array().cloned().unwrap_or_default();
            rows.sort_by_key(|r| r["payload"]["chunk_ord"].as_u64().unwrap_or(0));
            for row in rows {
                if seen.insert(row["id"].to_string()) {
                    selected.push(row);
                }
            }
        } else if seen.insert(point["id"].to_string()) {
            selected.push(point.clone());
        }
    }
    let total = selected.len();
    let mut used = 0;
    let mut output = Vec::new();
    let mut sources = Vec::new();
    for point in selected {
        let p = &point["payload"];
        let text = p["text"].as_str().unwrap_or("");
        let n = text.chars().count();
        // Never silently truncate a section and describe it as complete.
        if used + n > max_chars {
            continue;
        }
        used += n;
        let doc = document(
            context,
            &json!({"dataset_id":p["dataset_id"],"file_name":p["file_name"]}),
        )
        .await?;
        scoped_document(turn, &doc)?;
        if p["source_sha256"]
            .as_str()
            .is_some_and(|h| !h.is_empty() && doc["file_hash"] != h)
        {
            return Err("INDEX_CHANGED_DURING_SEARCH".into());
        }
        let page = p
            .get("source_page")
            .filter(|v| !v.is_null())
            .unwrap_or(&p["page"])
            .clone();
        let mut source = reference(&doc, page);
        source["qdrant_point_id"] = point["id"].clone();
        source["parent_id"] = p["parent_id"].clone();
        sources.push(source.clone());
        output.push(json!({"text":text,"source":source,"chunk_ord":p["chunk_ord"],"source_ref":p["source_ref"]}));
    }
    if output.is_empty() {
        return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
    }
    if store
        .revision(
            context,
            collection.as_str().ok_or("INDEX_COLLECTION_MISMATCH")?,
        )
        .await?
        != revision
    {
        return Err("INDEX_CHANGED_DURING_SEARCH".into());
    }
    let included = output.len();
    Ok(envelope(
        name,
        "indexed_sections",
        json!({"passages":output,"index_revision":revision,"coverage":if included==total{"complete"}else{"bounded"},"total_fragments":total,"included_fragments":included}),
        sources,
    ))
}
pub(super) async fn read(context: Arc<Context>, turn: &Prepared, args: Value) -> Result<Value> {
    let doc = document(&context, &args).await?;
    scoped_document(turn, &doc)?;
    let (config, _, _, _) = configuration(context.clone(), false).await?;
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let collection = config["collection"]
        .as_str()
        .ok_or("INDEX_COLLECTION_MISMATCH")?;
    let revision = store.revision(&context, collection).await?;
    let mut points = Vec::new();
    let mut offset = Value::Null;
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0;
    loop {
        let mut query = json!({"filter":{"must":[{"key":"dataset_id","match":{"value":doc["dataset_id"]}},{"key":"file_name","match":{"value":doc["file_name"]}}],"must_not":[{"key":"node_role","match":{"value":"navigation"}},{"key":"evidence_admissible","match":{"value":false}}]},"limit":128,"with_payload":true,"with_vector":false});
        if !offset.is_null() {
            query["offset"] = offset.clone();
        }
        let result = store
            .read_owned(&context, collection, "scroll", query)
            .await?;
        let rows = result["points"].as_array().ok_or("INDEX_READ_INVALID")?;
        for row in rows {
            if row["payload"]["dataset_id"] != doc["dataset_id"]
                || row["payload"]["file_name"] != doc["file_name"]
            {
                return Err("TOOL_SCOPE_VIOLATION".into());
            }
            bytes += row.to_string().len();
            if points.len() >= 4096 || bytes > 16 * 1024 * 1024 {
                return Err("SOURCE_READ_LIMIT".into());
            }
            points.push(row.clone());
        }
        offset = result["next_page_offset"].clone();
        if offset.is_null() {
            break;
        }
        if rows.is_empty() || !seen.insert(offset.to_string()) {
            return Err("INDEX_READ_INVALID".into());
        }
    }
    let clause = args["clause"].as_str().unwrap_or("");
    if !clause.is_empty() {
        les_core::chat_request::clause_heading("", clause)?;
        points.retain(|p| {
            let heading = p["payload"]["section_heading"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| p["payload"]["parent_heading"].as_str())
                .unwrap_or("");
            les_core::chat_request::clause_heading(heading, clause).unwrap_or(false)
        });
        let headings = points
            .iter()
            .map(|p| {
                json!([
                    p["payload"]["section_heading"],
                    p["payload"]["source_sha256"]
                ])
                .to_string()
            })
            .collect::<std::collections::HashSet<_>>();
        if headings.len() > 1 {
            return Err("SOURCE_CLAUSE_AMBIGUOUS".into());
        }
    } else if let Some(query) = args["q"].as_str().filter(|s| !s.trim().is_empty()) {
        let query = query.to_owned();
        points = tokio::task::spawn_blocking(move || -> Result<Vec<Value>> {
            let mut texts = vec![query];
            texts.extend(
                points
                    .iter()
                    .map(|p| p["payload"]["text"].as_str().unwrap_or("").into()),
            );
            let tokens = les_core::lexical::current(&texts)?;
            let terms = tokens["documents"][0]["terms"]
                .as_object()
                .ok_or("INDEX_QUERY_INVALID")?;
            let mut ranked: Vec<_> = points
                .into_iter()
                .enumerate()
                .map(|(i, p)| {
                    let count = terms
                        .keys()
                        .filter(|key| tokens["documents"][i + 1]["terms"].get(*key).is_some())
                        .count();
                    (count, p)
                })
                .collect();
            ranked.retain(|(score, _)| *score > 0);
            ranked.sort_by_key(|(n, p)| {
                (
                    std::cmp::Reverse(*n),
                    p["payload"]["chunk_ord"].as_u64().unwrap_or(0),
                )
            });
            Ok(ranked.into_iter().map(|(_, p)| p).collect())
        })
        .await
        .map_err(|_| "SOURCE_READ_FAILED")??;
    }
    if points.is_empty() {
        return Err("SOURCE_SECTION_NOT_FOUND".into());
    }
    points.truncate(args["limit"].as_u64().unwrap_or(6).clamp(1, 16) as usize);
    points.sort_by_key(|p| p["payload"]["chunk_ord"].as_u64().unwrap_or(0));
    passages(
        &context,
        turn,
        &config["collection"],
        points,
        revision,
        args["max_chars"].as_u64().unwrap_or(3000).clamp(1, 16000) as usize,
        "read_source",
    )
    .await
}
