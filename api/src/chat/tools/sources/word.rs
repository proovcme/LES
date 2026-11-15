//! Registered original -> existing native format worker -> lossless reading units.
//! This reader also upgrades old indexes without reconstructing overlapping text.
use super::*;
use std::{
    collections::VecDeque,
    fs::File,
    sync::{Mutex, OnceLock},
};
static CACHE: OnceLock<Mutex<VecDeque<(String, Value, usize)>>> = OnceLock::new();
mod saved;
mod window;
/// Automatic chat admission reads Word originals before the common selector.
/// The index anchor locates the document; it is never relabelled as its body.
pub(super) async fn hydrate(
    context: &Context,
    turn: &SourceContext,
    collection: &str,
    points: Vec<Value>,
    mut hydrated: Value,
    surrounding_chars: usize,
) -> Result<(Vec<Value>, Value)> {
    let mode = window::Mode::requested()?;
    let mut statistics = json!({"schema":"les.word-read-window.v1","mode":mode.name(),
        "documents":0,"original_units":0,"retained_units":0,"matched_units":0,
        "unresolved_anchors":0,"omitted_units":0});
    let mut output = Vec::new();
    let mut text_bytes = 0usize;
    let mut documents = std::collections::HashSet::new();
    let anchors = points.clone();
    for point in points {
        let p = &point["payload"];
        let name = p["file_name"].as_str().unwrap_or("");
        if !matches!(
            name.rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "doc" | "docx"
        ) {
            output.push(point);
            continue;
        }
        let key = json!([p["dataset_id"], p["file_name"], p["source_sha256"]]).to_string();
        if !documents.insert(key.clone()) {
            continue;
        }
        let doc = document(
            context,
            &json!({"dataset_id":p["dataset_id"],"file_name":p["file_name"]}),
        )
        .await?;
        scoped_document(turn, &doc)?;
        if p["source_sha256"] != doc["file_hash"] {
            return Err("SOURCE_REVISION_CHANGED".into());
        }
        // This runs in passages' async owner; parsing and hashing stay off Tokio.
        let owned_context = context.state_root.clone();
        let original = doc.clone();
        let cached = if saved::enabled()? {
            saved::load(context, &doc, collection).await?
        } else {
            None
        };
        let view = match cached {
            Some(view) => Ok(view),
            None => {
                if saved::enabled()? {
                    eprintln!(
                        "{}",
                        json!({"event":"saved_word_read_legacy_original","doc_id":doc["id"]})
                    );
                }
                tokio::task::spawn_blocking(move || snapshot_at(&owned_context, &original, 30000))
                    .await
                    .map_err(|_| "SOURCE_READ_FAILED")?
            }
        };
        let view = match view {
            Ok(value) => value,
            Err(failure)
                if retrieval::failures::record(
                    &mut hydrated,
                    reference(&doc, Value::Null),
                    &failure.code,
                ) =>
            {
                continue;
            }
            Err(failure) => return Err(failure),
        };
        // A legacy text index can locate a document without a compatible unit
        // hash. Retain that document prior separately; never invent a unit hit.
        let source_rank = anchors
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                let a = &a["payload"];
                a["dataset_id"] == doc["dataset_id"]
                    && a["file_name"] == doc["file_name"]
                    && a["source_sha256"] == doc["file_hash"]
                    && a["retrieval_expansion"] != true
            })
            .map(|(i, a)| a["payload"]["retrieval_rank"].as_u64().unwrap_or(i as u64))
            .min();
        let plan = window::plan(&view, &anchors, &doc, surrounding_chars, mode)?;
        for field in [
            "original_units",
            "retained_units",
            "matched_units",
            "unresolved_anchors",
            "omitted_units",
        ] {
            statistics[field] = json!(
                statistics[field].as_u64().unwrap_or(0)
                    + plan.statistics()[field].as_u64().unwrap_or(0)
            );
        }
        statistics["documents"] = json!(statistics["documents"].as_u64().unwrap_or(0) + 1);
        eprintln!(
            "{}",
            json!({"event":"word_read_scope","doc_id":doc["id"],
            "source_sha256":doc["file_hash"],"window":plan.statistics()})
        );
        if mode == window::Mode::Section && plan.unresolved > 0 {
            retrieval::failures::record(
                &mut hydrated,
                reference(&doc, Value::Null),
                "WORD_ANCHOR_UNRESOLVED",
            );
        }
        for read_offset in plan.indices.iter().copied() {
            let unit = &view["units"][read_offset];
            let owner = plan
                .owner(read_offset)
                .map(|a| &anchors[a.index])
                .unwrap_or(&point);
            let owner_payload = &owner["payload"];
            text_bytes += unit["text"].as_str().map_or(0, str::len);
            if output.len() >= les_core::context_selection::MAX_SOURCE_UNITS
                || text_bytes > les_core::context_selection::MAX_SOURCE_TEXT_BYTES
            {
                return Err("SOURCE_READ_LIMIT".into());
            }
            let mut payload = json!({"dataset_id":doc["dataset_id"],"file_name":doc["file_name"],"source_sha256":doc["file_hash"],
                "text":unit["text"],"reading_representation":"word_structure","word_physical":true,
                "word_snapshot_sha256":view["view_sha256"],"retrieval_anchor_id":owner_payload.get("retrieval_anchor_id").unwrap_or(&owner["id"]),
                "chunk_ord":unit["ordinal"],"section_heading":unit["heading"],"source_unit_id":unit["ordinal"]});
            // This is the original stream position, not the filtered window index.
            payload["word_read_offset"] = json!(read_offset);
            if view["saved_units"] == true {
                payload["reading_origin"] = json!("published_canonical_unit");
            }
            // An original document is not a dense hit at every paragraph.
            // Only a compatible indexed unit can inherit an anchor's rank.
            let matched = plan.matches.get(&read_offset).map(|a| a.rank);
            payload["retrieval_expansion"] = json!(matched.is_none());
            payload["retrieval_rank"] = json!(matched);
            payload["retrieval_source_rank"] = json!(source_rank);
            for field in [
                "ocr",
                "visual_content",
                "unread_visual_count",
                "visual_coverage",
            ] {
                payload[field] = view["metadata"][field].clone();
            }
            for field in [
                "ordinal",
                "canonical",
                "source_block_id",
                "source_parent_id",
                "source_heading_path",
                "table_id",
                "row_start",
                "row_end",
                "start_byte",
                "end_byte",
            ] {
                payload[field] = unit[field].clone();
            }
            output.push(json!({"id":format!("word:{}:{}:{}",doc["id"].as_str().unwrap_or(""),view["view_sha256"].as_str().unwrap_or(""),unit["ordinal"]),"payload":payload}));
        }
    }
    for group in ["sections", "documents"] {
        for section in hydrated[group].as_array_mut().into_iter().flatten() {
            if let Some(rows) = section["points"].as_array_mut() {
                rows.retain(|row| {
                    !documents.contains(
                        &json!([
                            row["payload"]["dataset_id"],
                            row["payload"]["file_name"],
                            row["payload"]["source_sha256"]
                        ])
                        .to_string(),
                    )
                });
            }
        }
    }
    if !hydrated["read_plan"].is_object() {
        hydrated["read_plan"] = json!({});
    }
    hydrated["read_plan"]["word"] = statistics;
    Ok((output, hydrated))
}
pub(super) fn supports(doc: &Value) -> bool {
    doc["file_name"].as_str().is_some_and(|n| {
        matches!(
            n.rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "doc" | "docx"
        )
    })
}
fn snapshot(context: &Context, doc: &Value) -> Result<Value> {
    snapshot_at(&context.state_root, doc, 120000)
}
fn verify_at(state: &std::path::Path, doc: &Value) -> Result<()> {
    let path = les_core::document_source::resolve(
        doc["source_path"].as_str().unwrap_or(""),
        doc["dataset_id"]
            .as_str()
            .ok_or("SOURCE_IDENTITY_INVALID")?,
        doc["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?,
        &state.join("storage/datasets"),
    )?;
    let actual = les_core::pdf_source::fingerprint(
        &mut File::open(path).map_err(|_| "SOURCE_UNAVAILABLE")?,
    )?;
    if doc["file_hash"] != actual {
        return Err("SOURCE_REVISION_CHANGED".into());
    }
    Ok(())
}
fn snapshot_at(state: &std::path::Path, doc: &Value, timeout_ms: u64) -> Result<Value> {
    let path = les_core::document_source::resolve(
        doc["source_path"].as_str().unwrap_or(""),
        doc["dataset_id"]
            .as_str()
            .ok_or("SOURCE_IDENTITY_INVALID")?,
        doc["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?,
        &state.join("storage/datasets"),
    )?;
    let verify = || -> Result<()> {
        let actual = les_core::pdf_source::fingerprint(
            &mut File::open(&path).map_err(|_| "SOURCE_UNAVAILABLE")?,
        )?;
        if doc["file_hash"] != actual {
            return Err("SOURCE_REVISION_CHANGED".into());
        }
        Ok(())
    };
    verify()?;
    let mut input = json!({"schema":"les.document-convert.v1","path":path,"source_sha256":doc["file_hash"],"owned_state":state});
    crate::document_dispatch::freeze_ocr(&mut input, state);
    if crate::document_parser::enabled()? {
        input["word_parser"] = json!("docling");
    }
    let key = json!([
        state,
        crate::document_cache::identity(&input)?,
        "les.word-reading.v2"
    ])
    .to_string();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "SOURCE_READ_FAILED")?;
    if let Some(i) = cache.iter().position(|(k, _, _)| *k == key) {
        let entry = cache.remove(i).unwrap();
        let value = entry.1.clone();
        cache.push_back(entry);
        verify()?;
        return Ok(value);
    }
    // A slow cold document must not block cached reads of another original.
    // The format worker has its own supervised queue and owns conversion I/O.
    drop(cache);
    let program = std::env::current_exe().map_err(|_| "SOURCE_READER_UNAVAILABLE")?;
    let started = std::time::Instant::now();
    eprintln!(
        "{}",
        json!({"event":"native_word_read_started","doc_id":doc["id"]})
    );
    let full = if let Some(value) = crate::document_cache::load(&input)? {
        value
    } else {
        let reply = les_core::run_native_documents(
            &json!({"schema":"les.worker-host.v1","program":program,"directory":program.parent(),
        "args":["--serve-document-formats"],"context":"les.format-dispatch.v1","timeout_ms":timeout_ms,"input":input}),
        )?;
        if reply["status"] != "completed" || reply["result"]["status"] != "completed" {
            eprintln!(
                "{}",
                json!({"event":"native_word_read_failed","doc_id":doc["id"],"code":reply["result"]["code"],
            "host_code":reply["code"],"elapsed_ms":started.elapsed().as_millis(),"worker_pid":reply["worker_pid"],"diagnostics":reply["diagnostics"]})
            );
            return Err(reply["result"]["code"]
                .as_str()
                .or(reply["code"].as_str())
                .unwrap_or("SOURCE_READ_FAILED")
                .into());
        }
        reply["result"]["value"].clone()
    };
    if full["source_sha256"] != doc["file_hash"] {
        return Err("SOURCE_REVISION_CHANGED".into());
    }
    let mut value = les_core::document_reading(
        &json!({"text":full["text"],"structure":full["metadata"]["docling_structure"],"max_chars":4000}),
    )?;
    value["metadata"] = full["metadata"].clone();
    if let Some(meta) = value["metadata"].as_object_mut() {
        meta.remove("docling_structure");
    }
    verify()?;
    eprintln!(
        "{}",
        json!({"event":"native_word_read_completed","doc_id":doc["id"],"elapsed_ms":started.elapsed().as_millis(),"units":value["units"].as_array().map_or(0,Vec::len)})
    );
    let size = value.to_string().len();
    let mut cache = CACHE
        .get()
        .unwrap()
        .lock()
        .map_err(|_| "SOURCE_READ_FAILED")?;
    // Concurrent readers can finish the same immutable source. Keep one entry.
    cache.retain(|(k, _, _)| k != &key);
    while cache.len() >= 16
        || cache.iter().map(|(_, _, bytes)| *bytes).sum::<usize>() + size > 16 * 1024 * 1024
    {
        if cache.pop_front().is_none() {
            break;
        }
    }
    if size <= 16 * 1024 * 1024 {
        cache.push_back((key, value.clone(), size));
    }
    Ok(value)
}
pub(super) async fn read(
    context: Arc<Context>,
    doc: Value,
    args: Value,
    revision: Value,
) -> Result<Value> {
    let owner = context.clone();
    let original = doc.clone();
    let cached = if saved::enabled()?
        && doc["status"] == "INDEXED"
        && context.index_store.is_some()
        && args["q"].as_str().is_none_or(str::is_empty)
        && args["clause"].as_str().is_none_or(str::is_empty)
    {
        let (config, _, _, _) = retrieval::configuration(context.clone(), false).await?;
        saved::load(
            &context,
            &doc,
            config["collection"]
                .as_str()
                .ok_or("INDEX_COLLECTION_MISMATCH")?,
        )
        .await?
    } else {
        None
    };
    let view = match cached {
        Some(value) => value,
        None => tokio::task::spawn_blocking(move || snapshot(&owner, &original))
            .await
            .map_err(|_| "SOURCE_READ_FAILED")??,
    };
    if view["saved_units"] == true
        && args["index_revision"]
            .as_str()
            .is_some_and(|r| view["index_revision"] != r)
    {
        return Err("INDEX_CHANGED_DURING_SEARCH".into());
    }
    if args["view_sha256"]
        .as_str()
        .is_some_and(|s| view["view_sha256"] != s)
    {
        return Err("SOURCE_VIEW_CHANGED".into());
    }
    let mut units = view["units"]
        .as_array()
        .cloned()
        .ok_or("DOCUMENT_STRUCTURE_INVALID")?;
    let document_units = units.len();
    let clause = args["clause"].as_str().unwrap_or("");
    if !clause.is_empty() {
        les_core::chat_request::clause_heading("", clause)?;
        units.retain(|u| {
            les_core::chat_request::clause_heading(u["heading"].as_str().unwrap_or(""), clause)
                .unwrap_or(false)
        });
    }
    if let Some(query) = args["q"].as_str().filter(|s| !s.trim().is_empty()) {
        let points = units
            .into_iter()
            .map(|u| json!({"payload":{"text":u["text"],"chunk_ord":u["ordinal"]},"unit":u}))
            .collect();
        units = retrieval::rank_units(points, query.to_owned())
            .await?
            .into_iter()
            .map(|p| p["unit"].clone())
            .collect();
    }
    if units.is_empty() {
        return Err("SOURCE_SECTION_NOT_FOUND".into());
    }
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    if offset > 0 && args["view_sha256"].as_str().is_none() {
        return Err("SOURCE_CONTINUATION_VIEW_REQUIRED".into());
    }
    if offset >= units.len() {
        return Err("SOURCE_OFFSET_INVALID".into());
    }
    let cap = args["max_chars"].as_u64().unwrap_or(6000).clamp(1, 16000) as usize;
    let limit = args["limit"].as_u64().unwrap_or(6).clamp(1, 16) as usize;
    let mut passages = Vec::new();
    let mut sources = Vec::new();
    let mut chars = 0;
    for u in units.iter().skip(offset).take(limit) {
        let text = u["text"].as_str().ok_or("DOCUMENT_STRUCTURE_INVALID")?;
        let cost = text.chars().count();
        if chars + cost > cap {
            break;
        }
        chars += cost;
        let mut source = reference(&doc, Value::Null);
        source["evidence_kind"] = json!("physical_word_section");
        source["reading_representation"] = json!("word_structure");
        for field in [
            "ocr",
            "visual_content",
            "unread_visual_count",
            "visual_coverage",
        ] {
            source[field] = view["metadata"][field].clone();
        }
        source["view_sha256"] = view["view_sha256"].clone();
        if view["saved_units"] == true {
            source["read_offset"] = json!(offset + sources.len());
            source["reading_origin"] = json!("published_canonical_unit");
        }
        for field in [
            "ordinal",
            "heading",
            "table_id",
            "row_start",
            "row_end",
            "start_byte",
            "end_byte",
        ] {
            source[field] = u[field].clone();
        }
        source["section_heading"] = u["heading"].clone();
        sources.push(source.clone());
        passages.push(json!({"text":text,"body_sha256":les_core::sha256_hex(u["body"].as_str().unwrap_or("").as_bytes()),
            "canonical":u["canonical"],"context_prefix_chars":u["context_prefix"].as_str().unwrap_or("").chars().count(),"source":source,"chunk_ord":u["ordinal"]}));
    }
    if passages.is_empty() {
        return Err("SOURCE_ATOMIC_UNIT_OVERFLOW".into());
    }
    let count = passages.len();
    let next = offset + count;
    let mut reply = envelope(
        "read_source",
        "physical_word_sections",
        json!({"passages":passages,"offset":offset,
        "next_offset":if next<units.len(){Some(next)}else{None},"total_fragments":units.len(),"included_fragments":count,
        "coverage":if offset==0&&next==units.len(){"complete"}else{"bounded"},"index_revision":revision,
        "view_sha256":view["view_sha256"],"extractor":view["metadata"]["extractor"],"ocr":view["metadata"]["ocr"],
        "unread_visual_count":view["metadata"]["unread_visual_count"]}),
        sources,
    );
    retrieval::bound_word(&mut reply)?;
    if view["saved_units"] == true {
        reply["result"]["index_revision"] = view["index_revision"].clone();
        reply["result"]["reading_origin"] = json!("published_canonical_unit");
        reply["result"]["coverage_scope"] = json!("published_reading_units");
        reply["result"]["complete_original"] = json!(false);
    }
    // Budget trimming changes the stream cursor, never skip a removed row.
    let kept = reply["result"]["passages"].as_array().map_or(0, Vec::len);
    reply["result"]["included_fragments"] = json!(kept);
    reply["result"]["next_offset"] = if offset + kept < units.len() {
        json!(offset + kept)
    } else {
        Value::Null
    };
    if kept < count {
        reply["result"]["coverage"] = json!("bounded");
    }
    retrieval::reading_scope(&mut reply, &args, document_units);
    Ok(reply)
}
