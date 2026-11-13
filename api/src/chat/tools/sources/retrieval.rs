//! Query-time dense/BM25/RRF followed by generation-checked parent reads.
use super::*;
use crate::index_batch::configuration::{identity, tokenizer, Settings};
mod matching;
mod packing;
mod views;

/// A materialized page view replaces all its search chunks at read time. Count
/// that page once *before* applying the document quota and shortlist limit.
/// Pages without such a view keep their separate chunks and continuations.
fn reading_units(points: &[Value]) -> Vec<Value> {
    let page = |p: &Value| {
        let p = &p["payload"];
        p["page"].as_u64().map(|page| {
            json!([p["dataset_id"], p["file_name"], p["source_sha256"], page]).to_string()
        })
    };
    let materialized: std::collections::HashSet<_> = points
        .iter()
        .filter(|p| {
            p["payload"]["pdf_page_view_schema"] == "les.pdf-page-view.v1"
                && p["payload"]["pdf_page_view"]
                    .as_str()
                    .is_some_and(|v| !v.is_empty())
        })
        .filter_map(&page)
        .collect();
    let mut seen = std::collections::HashSet::new();
    points
        .iter()
        .filter(|p| match page(p) {
            Some(key) if materialized.contains(&key) => seen.insert(key),
            _ => true,
        })
        .cloned()
        .collect()
}
/// Limit repeated search seeds, never the continuation pages read afterwards.
/// Dataset, full path and revision distinguish different physical sources.
fn diverse(points: &[Value], per_document: usize) -> Vec<Value> {
    let mut counts = std::collections::HashMap::<String, usize>::new();
    points
        .iter()
        .filter(|point| {
            let p = &point["payload"];
            let key = json!([p["dataset_id"], p["file_name"], p["source_sha256"]]).to_string();
            let count = counts.entry(key).or_default();
            *count += 1;
            *count <= per_document
        })
        .cloned()
        .collect()
}
fn query_text(settings: &Settings, query: &str) -> Result<String> {
    match settings.get("RAG_QUERY_EMBEDDING_MODE","raw-v1") {
        "raw-v1"=>Ok(query.into()),
        // The same explicit asymmetric contract as the legacy adapter. It changes
        // only query embeddings; indexed document vectors are never rewritten.
        "qwen-retrieval-v1" if settings.get("LES_EMBED_PROFILE","legacy")=="qwen"=>Ok(format!("Instruct: Given a search query, retrieve relevant passages from the selected corpus\nQuery: {query}")),
        _=>Err("QUERY_EMBEDDING_CONTRACT_INVALID".into())
    }
}
pub(crate) async fn configuration(
    context: Arc<Context>,
    embedding: bool,
) -> Result<(Value, Value, String, bool)> {
    scoped_configuration(context, embedding, None).await
}
async fn scoped_configuration(
    context: Arc<Context>,
    embedding: bool,
    remote_scope: Option<Value>,
) -> Result<(Value, Value, String, bool)> {
    tokio::task::spawn_blocking(move||->Result<_>{
        let settings=Settings::load(&context.state_root)?;
        let (_,path)=identity(&context,&settings)?;
        let bytes=std::fs::read(path).map_err(|_|"INDEX_NOT_CONFIGURED")?;
        if bytes.len()>1024*1024{return Err("INDEX_SETTINGS_INVALID".into());}
        let contract:Value=serde_json::from_slice(&bytes).map_err(|_|"INDEX_SETTINGS_INVALID")?;
        let mut frozen=les_core::index_configuration::execute(&json!({"contract":contract,"settings":settings.core_values(),"tokenizer":tokenizer(&settings,&contract)?}))?;
        let fusion=settings.get("RAG_HYBRID_FUSION","rrf");
        if !matches!(fusion,"rrf"|"dbsf") { return Err("FUSION_METHOD_INVALID".into()); }
        frozen["fusion"]=json!(fusion);
        if !embedding {return Ok((frozen,Value::Null,String::new(),false));}
        let execution=super::super::super::unwrap(crate::library::catalog(&context.database,"model_execution_snapshot",json!({"role":"embeddings"})))?;
        if execution.is_null(){return Err("EMBEDDING_MODEL_NOT_ASSIGNED".into());}
        if context.public_chat_rust&&execution["revision"]["locality"]=="remote"{
            let scope=remote_scope.as_ref().ok_or("CLOUD_EMBEDDING_SCOPE_UNVERIFIED")?;
            super::super::super::egress::check(&context,scope)?;
        }
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
/// The coordinator and recovery use the same initial read policy as tools.
pub(in crate::chat) async fn initial(context: Arc<Context>, turn: &Prepared) -> Result<Value> {
    // This is only an IO ceiling. The shared model packer owns final admission
    // with the actual prompt, schemas and dialogue. A fixed 6000-char pre-cut
    // discarded continuation pages before that packer could consider them.
    let profile = &turn.connection.profile;
    let chars = profile["input_token_limit"]
        .as_u64()
        .ok_or("CHAT_CONTEXT_INVALID")?
        .saturating_sub(
            profile["generation_reserve_tokens"]
                .as_u64()
                .ok_or("CHAT_CONTEXT_INVALID")?,
        )
        .saturating_sub(
            profile["safety_reserve_tokens"]
                .as_u64()
                .ok_or("CHAT_CONTEXT_INVALID")?,
        )
        .saturating_mul(2)
        .clamp(1, 16000);
    search_with_navigation(
        context,
        turn,
        json!({"q":turn.request["question"],"max_chars":chars}),
        false,
    )
    .await
}
pub(in crate::chat) async fn search(
    context: Arc<Context>,
    turn: &Prepared,
    args: Value,
) -> Result<Value> {
    search_with_navigation(context, turn, args, true).await
}
async fn search_with_navigation(
    context: Arc<Context>,
    turn: &Prepared,
    args: Value,
    navigation: bool,
) -> Result<Value> {
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
    let query = args["q"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 20000)
        .ok_or("INDEX_QUERY_INVALID")?;
    if args["view"] == "documents" {
        return super::catalog::search(context, turn, &args, &datasets).await;
    }
    let remote_scope = turn
        .egress_tag
        .as_ref()
        .map(|_| turn.scope["resolved_dataset_ids"].clone());
    let (config, revision, prefix, allow_remote) =
        scoped_configuration(context.clone(), true, remote_scope).await?;
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
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let candidate_limit = policy["retrieval_candidate_k"].as_u64().unwrap_or(64) as usize;
    if offset > candidate_limit {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    let request = json!({"collection":config["collection"],"text":query,"vector":vector,"dense_name":config["vector_names"]["dense"],
        "limit":policy["retrieval_candidate_k"],"rank_constant":policy["rrf_rank_constant"],"fusion":config["fusion"],"preserve_heads":!navigation,"filters":{"dataset_ids":datasets,"doc_filter":files},"hierarchy":navigation});
    for attempt in 0..16 {
        let result: Result<Value> = async {
            let result = store.hybrid_owned(&context, request.clone()).await?;
            let raw_points = result["points"].as_array().ok_or("INDEX_READ_INVALID")?;
            // Diversify the automatic shortlist. Explicit tool pagination must
            // still expose every ranked match, including later pages of one file.
            let ranked = if navigation { raw_points.clone() } else {
                diverse(&reading_units(raw_points), policy["document_diversity_k"].as_u64().unwrap_or(2) as usize)
            };
            let points: Vec<Value> = ranked
                .iter()
                .skip(offset).take(limit)
                .cloned()
                .collect();
            // These identities remain discoverable even when the corresponding
            // text did not fit. They are navigation, never admitted evidence.
            let mut candidates = Vec::new();
            let mut documents = std::collections::HashSet::new();
            // Discovery must describe the searched pool, not only the few hits
            // whose text was read. A relevant lower-ranked document is still
            // reachable through its registered ID without repeating retrieval.
            for point in if navigation { &points } else { raw_points } {
                let p = &point["payload"];
                if !documents.insert(json!([p["dataset_id"],p["file_name"]]).to_string()) { continue; }
                let doc = document(&context, &json!({"dataset_id":p["dataset_id"],"file_name":p["file_name"]})).await?;
                scoped_document(turn, &doc)?;
                if p["source_sha256"].as_str().is_some_and(|hash| !hash.is_empty() && doc["file_hash"] != hash) {
                    // Selected text was checked by passages(). An unread stale
                    // catalog card cannot invalidate otherwise verified evidence.
                    continue;
                }
                candidates.push(json!({"doc_id":doc["id"],"dataset_id":doc["dataset_id"],
                    "path":doc["file_name"],
                    "file_name":doc["file_name"].as_str().unwrap_or("").rsplit(['/', '\\']).next().unwrap_or(""),
                    "source_sha256":doc["file_hash"],"page":p.get("source_page").filter(|v|!v.is_null()).unwrap_or(&p["page"]),
                    "evidence_admissible":false}));
                if candidates.len() >= 64 { break; }
            }
            let max_chars=args["max_chars"].as_u64().unwrap_or(3000).clamp(1,16000) as usize;
            let context_budget = if navigation {
                Some(
                    args["_context_budget_chars"]
                        .as_u64()
                        .map(|n| n as usize)
                        .unwrap_or(14000)
                        .min(14000),
                )
            } else {
                None
            };
            let mut reply=passages(&context,turn,&config["collection"],points.clone(),result["revision"].clone(),max_chars,"search_sources",context_budget,true).await?;
            reply["result"]["candidate_documents"] = json!(candidates);
            let total = ranked.len();
            reply["result"]["selection_policy"] = json!({"seed_limit":limit,"per_document_seed_limit":if navigation{Value::Null}else{policy["document_diversity_k"].clone()},"raw_candidates":raw_points.len(),"eligible_candidates":total,"navigation_requested":navigation});
            // Diagnostics belong to the initial request receipt, never to the
            // model's repeated search-tool exchanges or their result budget.
            if !navigation {
                reply["result"]["fusion"] = result["fusion"].clone();
                reply["result"]["fusion_trace"] = json!(raw_points.iter().map(|p|json!({"id":p["id"],"score":p["score"],"provenance":p["retrieval_fusion"]})).collect::<Vec<_>>());
            }
            let next = offset + points.len();
            reply["result"]["offset"] = json!(offset);
            reply["result"]["total_candidates"] = json!(total);
            reply["result"]["next_offset"] = if next < total { json!(next) } else { Value::Null };
            let mut seen=std::collections::HashSet::new();
            reply["result"]["navigation_hints"]=json!(result["hierarchy_candidates"].as_array().into_iter().flatten()
                .filter(|p|seen.insert(json!([p["payload"]["dataset_id"],p["payload"]["file_name"],p["payload"]["parent_id"]]).to_string()))
                .take(2).map(|p|json!({"dataset_id":p["payload"]["dataset_id"],"doc_name":p["payload"]["file_name"],
                    "parent_id":p["payload"]["parent_id"],"evidence_admissible":false})).collect::<Vec<_>>());
            if navigation {
                fit_envelope_budget(&mut reply, 22500, 240)?;
                if !points.is_empty() {
                    let delivered = reply["sources"].as_array().cloned().unwrap_or_default();
                    let mut consumed = 0usize;
                    for seed in &points {
                        let sp = &seed["payload"];
                        let present = delivered.iter().any(|s| {
                            s["qdrant_point_id"] == seed["id"]
                                || (s["reading_representation"] == "pdf_cells"
                                    && s["dataset_id"] == sp["dataset_id"]
                                    && s["doc_name"] == sp["file_name"]
                                    && s["source_sha256"] == sp["source_sha256"]
                                    && s["page"] == sp["page"])
                        });
                        if present {
                            consumed += 1;
                        } else {
                            break;
                        }
                    }
                    let next = offset + consumed.max(1);
                    reply["result"]["next_offset"] =
                        if next < total { json!(next) } else { Value::Null };
                }
            }
            Ok(reply)
        }
        .await;
        match result {
            Err(code)
                if attempt < 15
                    && matches!(
                        code.code.as_str(),
                        "INDEX_UPDATE_BUSY"
                            | "INDEX_RECOVERY_REQUIRED"
                            | "INDEX_CHANGED_DURING_SEARCH"
                            | "INDEX_SPARSE_STALE"
                            | "INDEX_RECOVERING"
                    ) =>
            {
                if attempt == 0 {
                    eprintln!(
                        "{}",
                        json!({"event":"search_waiting_for_publication","reason":code.code})
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            _ => return result,
        }
    }
    unreachable!()
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn projected_page_duplicates_do_not_spend_the_other_page_slot() {
        let mut first = point(1, "a", "source.pdf", "r1");
        first["payload"]["page"] = json!(1);
        first["payload"]["pdf_page_view_schema"] = json!("les.pdf-page-view.v1");
        first["payload"]["pdf_page_view"] = json!("Complete first page");
        let mut tail = point(2, "a", "source.pdf", "r1");
        tail["payload"]["page"] = json!(1);
        let mut later = point(3, "a", "source.pdf", "r1");
        later["payload"]["page"] = json!(3);
        let rows = vec![tail.clone(), first.clone(), later.clone()];
        let units = reading_units(&rows);
        assert_eq!(diverse(&units, 2), vec![tail.clone(), later.clone()]);
        assert_eq!(rows.len(), 3);
        // Unprojected text must retain its continuation chunks.
        first["payload"]
            .as_object_mut()
            .unwrap()
            .remove("pdf_page_view_schema");
        assert_eq!(
            reading_units(&[tail.clone(), first.clone(), later.clone()]),
            vec![tail, first, later]
        );
    }
    fn point(id: usize, dataset: &str, path: &str, revision: &str) -> Value {
        json!({"id":id,"payload":{"dataset_id":dataset,"file_name":path,"source_sha256":revision}})
    }
    #[test]
    fn repeated_hits_cannot_hide_a_lower_ranked_document() {
        let rows = vec![
            point(1, "a", "one.pdf", "r1"),
            point(2, "a", "one.pdf", "r1"),
            point(3, "a", "one.pdf", "r1"),
            point(4, "a", "two.pdf", "r1"),
        ];
        assert_eq!(
            diverse(&rows, 2)
                .iter()
                .map(|r| r["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!(1), json!(2), json!(4)]
        );
        assert_eq!(rows.len(), 4);
    }
    #[test]
    fn names_do_not_merge_datasets_paths_or_revisions() {
        let rows = vec![
            point(1, "a", "one.pdf", "r1"),
            point(2, "b", "one.pdf", "r1"),
            point(3, "a", "nested/one.pdf", "r1"),
            point(4, "a", "one.pdf", "r2"),
        ];
        assert_eq!(diverse(&rows, 1), rows);
    }
    #[test]
    fn envelope_budget_guard_bounds_oversized_passages_and_candidates_within_24k_256() {
        let long_path = format!("{}Спецификация_оборудования.pdf", "Раздел_ПД_Подраздел/".repeat(12));
        let mut passages = Vec::new();
        let mut sources = Vec::new();
        for i in 0..16 {
            let source = json!({
                "doc_id": "doc-0",
                "dataset_id": "ds-1",
                "doc_name": long_path,
                "source_sha256": "a".repeat(64),
                "page": i + 1,
                "source_page": i + 1,
                "locator": {"source_sha256": "a".repeat(64), "source_page": i + 1},
                "url": format!("/api/documents/by-id/doc-0/raw?source_sha256={}#page={}", "a".repeat(64), i + 1),
                "qdrant_point_id": format!("pt-{i}"),
                "parent_id": format!("parent-{i}"),
                "source_ref": format!("{long_path}#page={}", i + 1),
                "context_unit": "document:doc-0:aaaa",
                "document_fragment_count": 16,
                "context_origin": "document"
            });
            sources.push(source.clone());
            passages.push(json!({
                "text": format!(
                    "{}ЗАПРЕЩАЕТСЯ эксплуатация блока #{i} без заземления.",
                    "Позиция спецификации и технические характеристики оборудования. ".repeat(18)
                ),
                "source": source,
                "chunk_ord": i,
                "source_ref": format!("{long_path}#page={}", i + 1)
            }));
        }
        let mut reply = envelope(
            "search_sources",
            "indexed_sections",
            json!({
                "passages": passages,
                "index_revision": "rev-1",
                "read_receipt": {
                    "schema": "les.indexed-read.v1",
                    "index_revision": "rev-1",
                    "point_ids": (0..16).map(|i| json!(format!("pt-{i}"))).collect::<Vec<_>>()
                },
                "candidate_documents": (0..64).map(|i| json!({
                    "doc_id": format!("cand-{i}"),
                    "dataset_id": "ds-1",
                    "path": long_path,
                    "file_name": "Спецификация_оборудования.pdf",
                    "source_sha256": "a".repeat(64),
                    "page": i + 1,
                    "evidence_admissible": false
                })).collect::<Vec<_>>(),
                "coverage": "complete",
                "total_fragments": 16,
                "included_fragments": 16
            }),
            sources,
        );
        assert!(canonical_chars(&reply) > 24000);
        fit_envelope_budget(&mut reply, 22500, 240).unwrap();
        assert!(canonical_chars(&reply) <= 22500);
        assert!(json_item_count(&reply) <= 240);
        assert_eq!(reply["result"]["coverage"], "bounded");
        let kept = reply["result"]["passages"].as_array().unwrap();
        assert!(!kept.is_empty() && kept.len() < 16);
        assert_eq!(reply["result"]["offset"], 0);
        assert_eq!(reply["result"]["next_offset"], kept.len());
        for (i, p) in kept.iter().enumerate() {
            let text = p["text"].as_str().unwrap();
            assert!(
                text.ends_with(&format!("ЗАПРЕЩАЕТСЯ эксплуатация блока #{i} без заземления.")),
                "passage #{i} was truncated mid-text"
            );
            assert!(p["source"].get("context_unit").is_none());
            assert!(p["source"].get("document_fragment_count").is_none());
            assert!(p["source"].get("context_origin").is_none());
        }
        for s in reply["sources"].as_array().unwrap() {
            assert!(s.get("context_unit").is_none());
            assert!(s.get("document_fragment_count").is_none());
            assert!(s.get("context_origin").is_none());
        }
        assert_eq!(
            kept.len(),
            reply["sources"].as_array().unwrap().len()
        );
        assert_eq!(
            kept.len(),
            reply["result"]["read_receipt"]["point_ids"]
                .as_array()
                .unwrap()
                .len()
        );
    }

    #[test]
    fn envelope_budget_never_truncates_single_passage_mid_text_and_rejects_unfitting_single_block() {
        let source = json!({
            "doc_id": "doc-0",
            "dataset_id": "ds-1",
            "doc_name": "Регламент.pdf",
            "source_sha256": "a".repeat(64),
            "page": 1,
            "source_page": 1,
            "locator": {"source_sha256": "a".repeat(64), "source_page": 1},
            "url": "/api/documents/by-id/doc-0/raw",
            "qdrant_point_id": "pt-0",
            "parent_id": "parent-0",
            "source_ref": "Регламент.pdf#page=1",
            "context_unit": "document:doc-0:aaaa",
            "document_fragment_count": 1,
            "context_origin": "document"
        });
        let full_text = format!(
            "{}ЗАПРЕЩАЕТСЯ подача напряжения при открытых дверцах шкафа.",
            "Технические условия эксплуатации. ".repeat(120)
        );
        let mut reply = envelope(
            "read_source",
            "indexed_sections",
            json!({
                "passages": [{
                    "text": full_text,
                    "source": source,
                    "chunk_ord": 0,
                    "source_ref": "Регламент.pdf#page=1"
                }],
                "index_revision": "rev-1",
                "read_receipt": {
                    "schema": "les.indexed-read.v1",
                    "index_revision": "rev-1",
                    "point_ids": ["pt-0"]
                },
                "coverage": "complete",
                "total_fragments": 1,
                "included_fragments": 1
            }),
            vec![source],
        );
        // When the single block fits in the envelope budget, its ending prohibition and
        // complete-document marker are preserved verbatim.
        fit_envelope_budget(&mut reply, 22500, 240).unwrap();
        assert_eq!(reply["result"]["passages"][0]["text"], full_text);
        assert_eq!(
            reply["result"]["passages"][0]["source"]["context_unit"],
            "document:doc-0:aaaa"
        );
        // When the budget is smaller than the single indivisible block, it is never cut mid-text.
        let err = fit_envelope_budget(&mut reply, 1500, 240).unwrap_err();
        assert_eq!(err.code, "CONTEXT_REQUIRED_SECTION_OVERFLOW");
        assert!(reply["result"]["passages"].as_array().unwrap().is_empty());
        assert!(reply["sources"].as_array().unwrap().is_empty());
    }
}
async fn passages(
    context: &Context,
    turn: &Prepared,
    collection: &Value,
    points: Vec<Value>,
    revision: Value,
    max_chars: usize,
    name: &str,
    context_budget: Option<usize>,
    expand_document: bool,
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
        &json!({"collection":collection,"revision":revision,"seeds":seeds,"scopes":scopes,
            "document_budget_chars":if expand_document{Some(max_chars)}else{None}}),
    )
    .await?;
    let (points, hydrated) = views::project(points, hydrated)?;
    let (selected, total) = packing::select(
        &points,
        &hydrated["sections"],
        &hydrated["documents"],
        max_chars,
        context_budget,
    );
    let selected_ids: std::collections::HashSet<_> =
        selected.iter().map(|p| p["id"].to_string()).collect();
    let mut output = Vec::new();
    let mut sources = Vec::new();
    for point in selected {
        let p = &point["payload"];
        let text = p["text"].as_str().unwrap_or("");
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
        if p["ocr"] == true {
            source["ocr"] = json!(true);
        }
        for field in ["visual_content", "unread_visual_count", "visual_coverage"] {
            if !p[field].is_null() {
                source[field] = p[field].clone();
            }
        }
        if p["visual_content"] == true {
            source["parts_url"] = json!(format!(
                "/api/documents/by-id/{}/parts?source_sha256={}",
                doc["id"].as_str().unwrap_or(""),
                doc["file_hash"].as_str().unwrap_or("")
            ));
        }
        for field in ["sheet_name", "row", "source_ref"] {
            if !p[field].is_null() {
                source[field] = p[field].clone();
            }
        }
        source["qdrant_point_id"] = point["id"].clone();
        source["parent_id"] = p["parent_id"].clone();
        if p["reading_representation"].is_string() {
            source["reading_representation"] = p["reading_representation"].clone();
        }
        if let Some(document) = hydrated["documents"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|d| {
                d["points"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|r| r["id"] == point["id"]))
            })
        {
            let rows = document["points"].as_array().unwrap();
            // Only a fully selected document becomes an atomic context unit.
            // Selection adds all its points contiguously or none as a unit.
            if rows
                .iter()
                .all(|r| selected_ids.contains(&r["id"].to_string()))
            {
                source["context_unit"] = json!(format!(
                    "document:{}:{}",
                    doc["id"].as_str().unwrap_or(""),
                    doc["file_hash"].as_str().unwrap_or("")
                ));
                source["document_fragment_count"] = json!(rows.len());
                source["context_origin"] = json!("document");
            }
        }
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
    let mut reply = envelope(
        name,
        "indexed_sections",
        json!({"passages":output,"index_revision":revision,
            "read_receipt":{"schema":"les.indexed-read.v1","index_revision":revision,
                "point_ids":sources.iter().map(|s|s["qdrant_point_id"].clone()).collect::<Vec<_>>()},
            "coverage":if included==total{"complete"}else{"bounded"},"total_fragments":total,"included_fragments":included}),
        sources,
    );
    if context_budget.is_some() {
        let cap = if name == "search_sources" {
            19500
        } else {
            22000
        };
        fit_envelope_budget(&mut reply, cap, 230)?;
    }
    Ok(reply)
}

fn json_item_count(value: &Value) -> usize {
    match value {
        Value::Object(map) => map.values().map(json_item_count).sum(),
        Value::Array(values) => values.len() + values.iter().map(json_item_count).sum::<usize>(),
        _ => 0,
    }
}

fn canonical_chars(value: &Value) -> usize {
    les_core::tool_execution::canonical_json(value)
        .map(|s| s.chars().count())
        .unwrap_or_else(|_| value.to_string().chars().count())
}

fn reconcile_document_units(reply: &mut Value, orig_passages: &[Value]) {
    let mut orig_doc_counts = std::collections::HashMap::<(String, String), usize>::new();
    let mut orig_unit_counts = std::collections::HashMap::<String, usize>::new();
    for p in orig_passages {
        let s = &p["source"];
        let doc_key = (
            s["doc_id"].as_str().unwrap_or("").to_owned(),
            s["source_sha256"].as_str().unwrap_or("").to_owned(),
        );
        *orig_doc_counts.entry(doc_key).or_default() += 1;
        if let Some(unit) = s["context_unit"].as_str().filter(|u| !u.is_empty()) {
            *orig_unit_counts.entry(unit.to_owned()).or_default() += 1;
        }
    }
    let mut kept_doc_counts = std::collections::HashMap::<(String, String), usize>::new();
    let mut kept_unit_counts = std::collections::HashMap::<String, usize>::new();
    if let Some(passages) = reply["result"]["passages"].as_array() {
        for p in passages {
            let s = &p["source"];
            let doc_key = (
                s["doc_id"].as_str().unwrap_or("").to_owned(),
                s["source_sha256"].as_str().unwrap_or("").to_owned(),
            );
            *kept_doc_counts.entry(doc_key).or_default() += 1;
            if let Some(unit) = s["context_unit"].as_str().filter(|u| !u.is_empty()) {
                *kept_unit_counts.entry(unit.to_owned()).or_default() += 1;
            }
        }
    }
    let is_complete = |s: &Value| -> bool {
        let doc_key = (
            s["doc_id"].as_str().unwrap_or("").to_owned(),
            s["source_sha256"].as_str().unwrap_or("").to_owned(),
        );
        let kept_doc = kept_doc_counts.get(&doc_key).copied().unwrap_or(0);
        let orig_doc = orig_doc_counts.get(&doc_key).copied().unwrap_or(kept_doc);
        let expected_doc = s["document_fragment_count"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or(orig_doc);
        let unit_ok = s["context_unit"]
            .as_str()
            .filter(|u| !u.is_empty())
            .is_none_or(|u| {
                let kept_u = kept_unit_counts.get(u).copied().unwrap_or(0);
                let orig_u = orig_unit_counts.get(u).copied().unwrap_or(kept_u);
                kept_u == orig_u && kept_u == expected_doc
            });
        kept_doc > 0 && kept_doc == orig_doc && kept_doc == expected_doc && unit_ok
    };
    if let Some(passages) = reply["result"]["passages"].as_array_mut() {
        for p in passages {
            if !is_complete(&p["source"]) {
                if let Some(obj) = p["source"].as_object_mut() {
                    obj.remove("context_unit");
                    obj.remove("document_fragment_count");
                    obj.remove("context_origin");
                }
            }
        }
    }
    if let Some(sources) = reply["sources"].as_array_mut() {
        for s in sources {
            if !is_complete(s) {
                if let Some(obj) = s.as_object_mut() {
                    obj.remove("context_unit");
                    obj.remove("document_fragment_count");
                    obj.remove("context_origin");
                }
            }
        }
    }
}

pub(crate) fn fit_envelope_budget(
    reply: &mut Value,
    max_chars: usize,
    max_items: usize,
) -> Result<()> {
    let orig_passages = reply["result"]["passages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    reconcile_document_units(reply, &orig_passages);
    if canonical_chars(reply) <= max_chars && json_item_count(reply) <= max_items {
        return Ok(());
    }
    let orig_sources = reply["sources"].as_array().cloned().unwrap_or_default();
    let orig_ids = reply["result"]["read_receipt"]["point_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let orig_candidates = reply["result"]
        .get("candidate_documents")
        .and_then(Value::as_array)
        .cloned();
    let orig_hints = reply["result"]
        .get("navigation_hints")
        .and_then(Value::as_array)
        .cloned();
    if orig_candidates.is_some() {
        reply["result"]["candidate_documents"] = json!([]);
    }
    if orig_hints.is_some() {
        reply["result"]["navigation_hints"] = json!([]);
    }
    if orig_passages.is_empty() {
        return Ok(());
    }
    let offset = reply["result"]["offset"].as_u64().unwrap_or(0) as usize;
    let total = (reply["result"]["total_candidates"]
        .as_u64()
        .or_else(|| reply["result"]["total_fragments"].as_u64())
        .unwrap_or(orig_passages.len() as u64) as usize)
        .max(offset + orig_passages.len());
    let mut kept_passages = Vec::new();
    let mut kept_sources = Vec::new();
    let mut kept_ids = Vec::new();
    let mut omitted_any = false;
    for i in 0..orig_passages.len() {
        kept_passages.push(orig_passages[i].clone());
        kept_sources.push(
            orig_sources
                .get(i)
                .cloned()
                .unwrap_or_else(|| orig_passages[i]["source"].clone()),
        );
        kept_ids.push(
            orig_ids
                .get(i)
                .cloned()
                .unwrap_or_else(|| orig_passages[i]["source"]["qdrant_point_id"].clone()),
        );
        reply["result"]["passages"] = json!(kept_passages);
        reply["sources"] = json!(kept_sources);
        if reply["result"]["read_receipt"].is_object() {
            reply["result"]["read_receipt"]["point_ids"] = json!(kept_ids);
        }
        reply["result"]["included_fragments"] = json!(kept_passages.len());
        let bounded = omitted_any
            || kept_passages.len() < orig_passages.len()
            || offset + kept_passages.len() < total;
        reply["result"]["coverage"] = json!(if bounded { "bounded" } else { "complete" });
        if bounded {
            reply["result"]["offset"] = json!(offset);
            let next = offset + kept_passages.len();
            reply["result"]["next_offset"] = if next < total {
                json!(next)
            } else {
                Value::Null
            };
        }
        reconcile_document_units(reply, &orig_passages);
        if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
            kept_passages.pop();
            kept_sources.pop();
            kept_ids.pop();
            omitted_any = true;
            // Preserve contiguous block order so next_offset continues without gaps.
            break;
        }
    }
    reply["result"]["passages"] = json!(kept_passages);
    reply["sources"] = json!(kept_sources);
    if reply["result"]["read_receipt"].is_object() {
        reply["result"]["read_receipt"]["point_ids"] = json!(kept_ids);
    }
    reply["result"]["included_fragments"] = json!(kept_passages.len());
    reconcile_document_units(reply, &orig_passages);
    if omitted_any || kept_passages.len() < orig_passages.len() || offset + kept_passages.len() < total
    {
        reply["result"]["coverage"] = json!("bounded");
        reply["result"]["offset"] = json!(offset);
        let next = offset + kept_passages.len();
        reply["result"]["next_offset"] = if next < total {
            json!(next)
        } else {
            Value::Null
        };
    }
    if kept_passages.is_empty() {
        return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
    }
    if let Some(hints) = orig_hints {
        let mut kept_hints = Vec::new();
        for hint in hints {
            kept_hints.push(hint);
            reply["result"]["navigation_hints"] = json!(kept_hints);
            if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
                kept_hints.pop();
                reply["result"]["navigation_hints"] = json!(kept_hints);
                break;
            }
        }
    }
    if let Some(candidates) = orig_candidates {
        let cap = if omitted_any { 16 } else { 64 };
        let mut kept_candidates = Vec::new();
        for cand in candidates.into_iter().take(cap) {
            kept_candidates.push(cand);
            reply["result"]["candidate_documents"] = json!(kept_candidates);
            if canonical_chars(reply) > max_chars || json_item_count(reply) > max_items {
                kept_candidates.pop();
                reply["result"]["candidate_documents"] = json!(kept_candidates);
                break;
            }
        }
    }
    Ok(())
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
    }
    let query = args["q"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && clause.is_empty());
    if let Some(query) = query {
        let query = query.to_owned();
        points = tokio::task::spawn_blocking(move || matching::rank(points, query))
            .await
            .map_err(|_| "SOURCE_READ_FAILED")??;
    }
    if points.is_empty() {
        return Err("SOURCE_SECTION_NOT_FOUND".into());
    }
    if query.is_none() {
        points.sort_by_key(|p| p["payload"]["chunk_ord"].as_u64().unwrap_or(0));
    }
    points = reading_units(&points);
    let read_offset = args["offset"].as_u64().unwrap_or(0) as usize;
    if read_offset >= points.len() {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let total_units = points.len();
    let limit = args["limit"].as_u64().unwrap_or(6).clamp(1, 16) as usize;
    let page_points: Vec<Value> = points.into_iter().skip(read_offset).take(limit).collect();
    // Admission follows query relevance. Reading order is presentation only.
    let mut reply = passages(
        &context,
        turn,
        &config["collection"],
        page_points,
        revision,
        args["max_chars"].as_u64().unwrap_or(3000).clamp(1, 16000) as usize,
        "read_source",
        args["_context_budget_chars"].as_u64().map(|n| n as usize),
        clause.is_empty() && read_offset == 0,
    )
    .await?;
    if let Some(rows) = reply["result"]["passages"].as_array_mut() {
        rows.sort_by_key(|p| p["chunk_ord"].as_u64().unwrap_or(0));
    }
    let included = reply["result"]["passages"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let total = (reply["result"]["total_fragments"]
        .as_u64()
        .unwrap_or(total_units as u64) as usize)
        .max(total_units);
    reply["result"]["total_fragments"] = json!(total);
    reply["result"]["offset"] = json!(read_offset);
    if read_offset > 0 || read_offset + included < total {
        reply["result"]["coverage"] = json!("bounded");
        reply["result"]["next_offset"] = json!(read_offset + included);
    } else {
        reply["result"]["next_offset"] = Value::Null;
    }
    Ok(reply)
}
