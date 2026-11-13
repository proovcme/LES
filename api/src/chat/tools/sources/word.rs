//! Registered original -> existing native format worker -> lossless reading units.
//! This reader also upgrades old indexes without reconstructing overlapping text.
use super::*;
use std::{
    collections::VecDeque,
    fs::File,
    sync::{Mutex, OnceLock},
};
static CACHE: OnceLock<Mutex<VecDeque<(String, Value)>>> = OnceLock::new();
/// Automatic chat admission reads Word originals before the common selector.
/// The index anchor locates the document; it is never relabelled as its body.
pub(super) async fn hydrate(
    context: &Context,
    turn: &SourceContext,
    points: Vec<Value>,
    mut hydrated: Value,
) -> Result<(Vec<Value>, Value)> {
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
        let view =
            tokio::task::spawn_blocking(move || snapshot_at(&owned_context, &original, 30000))
                .await
                .map_err(|_| "SOURCE_READ_FAILED")?;
        let view = match view {
            Ok(value) => value,
            Err(failure)
                if retrieval::failures::record(
                    &mut hydrated,
                    reference(&doc, Value::Null),
                    &failure.code,
                ) =>
            {
                continue
            }
            Err(failure) => return Err(failure),
        };
        for unit in view["units"]
            .as_array()
            .ok_or("DOCUMENT_STRUCTURE_INVALID")?
        {
            text_bytes += unit["text"].as_str().map_or(0, str::len);
            if output.len() >= les_core::context_selection::MAX_SOURCE_UNITS
                || text_bytes > les_core::context_selection::MAX_SOURCE_TEXT_BYTES
            {
                return Err("SOURCE_READ_LIMIT".into());
            }
            let mut payload = json!({"dataset_id":doc["dataset_id"],"file_name":doc["file_name"],"source_sha256":doc["file_hash"],
                "text":unit["text"],"reading_representation":"word_structure","word_physical":true,
                "word_snapshot_sha256":view["view_sha256"],"retrieval_anchor_id":point["id"],
                "chunk_ord":unit["ordinal"],"section_heading":unit["heading"],"source_unit_id":unit["ordinal"]});
            // An original document is not a dense hit at every paragraph.
            // Only a compatible indexed unit can inherit an anchor's rank.
            let matched = anchors
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    let a = &a["payload"];
                    a["dataset_id"] == doc["dataset_id"]
                        && a["file_name"] == doc["file_name"]
                        && a["source_sha256"] == doc["file_hash"]
                        && a["word_reading_schema"] == "les.document-reading.v1"
                        && a["source_unit_id"] == unit["ordinal"]
                })
                .map(|(i, a)| a["payload"]["retrieval_rank"].as_u64().unwrap_or(i as u64))
                .min();
            payload["retrieval_expansion"] = json!(matched.is_none());
            payload["retrieval_rank"] = json!(matched);
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
    let key = json!([state, doc["file_hash"], "les.word-reading.v1"]).to_string();
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "SOURCE_READ_FAILED")?;
    if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
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
    let mut input = json!({"schema":"les.document-convert.v1","path":path,"source_sha256":doc["file_hash"],"owned_state":state});
    crate::document_dispatch::freeze_ocr(&mut input, state);
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
    let full = &reply["result"]["value"];
    if full["source_sha256"] != doc["file_hash"] {
        return Err("SOURCE_REVISION_CHANGED".into());
    }
    let mut value = les_core::document_reading(&json!({"text":full["text"],"max_chars":4000}))?;
    value["metadata"] = full["metadata"].clone();
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
    cache.retain(|(k, _)| k != &key);
    while cache.len() >= 16
        || cache
            .iter()
            .map(|(_, v)| v.to_string().len())
            .sum::<usize>()
            + size
            > 16 * 1024 * 1024
    {
        if cache.pop_front().is_none() {
            break;
        }
    }
    if size <= 16 * 1024 * 1024 {
        cache.push_back((key, value.clone()));
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
    let view = tokio::task::spawn_blocking(move || snapshot(&owner, &original))
        .await
        .map_err(|_| "SOURCE_READ_FAILED")??;
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
            "context_prefix_chars":u["context_prefix"].as_str().unwrap_or("").chars().count(),"source":source,"chunk_ord":u["ordinal"]}));
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
    Ok(reply)
}
