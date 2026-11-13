//! Own complete batch control: selection, tickets, generations, progress, retry and publication outcomes.
//! Bridges supply document preparation, embedding IO and the native storage transport; never batch decisions.
mod policy;
mod preparation;
use preparation::{fingerprint, Nodes, Spools};
mod store;
use serde_json::{json, Value};
use std::{cell::RefCell, path::Path, time::Instant};
use store::{Batch, Result};
const INVALID: &str = "INDEX_BATCH_INVALID";

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().filter(|v| !v.is_empty()).ok_or(INVALID)
}
fn error(code: &str) -> Value {
    json!({"code":code,"message":policy::message(code),"error_type":"NativeIndexError"})
}
fn exchange(
    bridge: &impl Fn(u8, &Value) -> Result<Value>,
    operation: u8,
    value: &Value,
) -> std::result::Result<Value, Value> {
    match bridge(operation, value) {
        Ok(value) if value.get("error").is_some() => Err(value["error"].clone()),
        Ok(value) => Ok(value),
        Err(code) => Err(error(code)),
    }
}
fn storage(
    bridge: &impl Fn(u8, &Value) -> Result<Value>,
    request: &Value,
    action: &str,
    fields: Value,
) -> std::result::Result<Value, Value> {
    let mut body = json!({"action":action,"collection":request["collection"]});
    body.as_object_mut()
        .unwrap()
        .extend(fields.as_object().cloned().unwrap_or_default());
    exchange(bridge, 2, &body)
}
fn recover_storage(
    bridge: &impl Fn(u8, &Value) -> Result<Value>,
    request: &Value,
) -> std::result::Result<(), Value> {
    let value = storage(bridge, request, "recover_file", json!({}))?;
    // Server and Edge own different journal paths. Only the storage owner can
    // confirm recovery; a successful HTTP exchange is insufficient.
    if matches!(value["status"].as_str(), Some("idle" | "recovered")) {
        Ok(())
    } else {
        Err(error("INDEX_RECOVERY_INCOMPLETE"))
    }
}

fn convert_document(
    request: &Value,
    input: &Value,
    bridge: &impl Fn(u8, &Value) -> Result<Value>,
) -> std::result::Result<Value, Value> {
    let path = Path::new(input["path"].as_str().ok_or_else(|| error(INVALID))?);
    let worker = &request["format_worker"];
    let mail_index =
        request["mail_index"] == true && crate::document::conversion::supports_mail(path);
    if !worker.is_object() || !(crate::document::conversion::supports(path) || mail_index) {
        return exchange(bridge, 0, input);
    }
    let mut chunking = request["chunking"].clone();
    chunking["tokenizer"] = request["node_preparation"]["tokenizer"].clone();
    let dataset = input["dataset_id"].as_str().ok_or_else(|| error(INVALID))?;
    let data_dir = Path::new(
        request["content_root"]
            .as_str()
            .ok_or_else(|| error(INVALID))?,
    )
    .join(dataset);
    let response=crate::worker_host::run_documents(&json!({"schema":"les.worker-host.v1","program":worker["program"],"directory":worker["directory"],
        "args":["--serve-document-formats"],"context":"les.format-dispatch.v1","timeout_ms":worker["timeout_ms"],
        "input":{"schema":"les.document-convert.v1","pipeline":"nodes","mail_index":mail_index,"path":path,"data_dir":data_dir,"dataset_id":dataset,"file_key":input["file_name"],
            "node_spool":input["node_spool"],"source_sha256":input["source_sha256"],"chunking":chunking,
            "minimum":worker["minimum"],"final_minimum":worker["final_minimum"],"table_row_limit":worker["table_row_limit"],"ocr":worker["ocr"],"ocr_unavailable":worker["ocr_unavailable"],
            "pdf_page_size":worker["pdf_page_size"].as_u64().unwrap_or(1800),"pdf_page_overlap":worker["pdf_page_overlap"].as_u64().unwrap_or(150)}})).map_err(error)?;
    let value = &response["result"];
    if response["status"] != "completed" || value["status"] != "completed" {
        let code = value["code"]
            .as_str()
            .or(response["code"].as_str())
            .unwrap_or("FORMAT_READ_FAILED");
        eprintln!(
            "{}",
            json!({"event":"native_format_failed","code":code,"exit_code":response["exit_code"],"worker_pid":response["worker_pid"],"diagnostics":response["diagnostics"]})
        );
        return Err(error(code));
    }
    if value["value"]["owner"] != "rust.format-dispatch" {
        return Err(error("FORMAT_WORKER_RESPONSE_INVALID"));
    }
    let mut result = value["value"].clone();
    result["route"]
        .as_object_mut()
        .ok_or_else(|| error(INVALID))?
        .remove("metadata");
    eprintln!(
        "{}",
        json!({"event":"native_document_prepared","format":path.extension().and_then(|s|s.to_str()),"nodes":result["count"],"owner":"rust.format-dispatch"})
    );
    Ok(result)
}

pub fn run(request: &Value, bridge: impl Fn(u8, &Value) -> Result<Value>) -> Result<Value> {
    if request["schema"] != "les.index-batch.v1" {
        return Err(INVALID);
    }
    let database = Path::new(text(request, "database")?);
    let root = Path::new(text(request, "content_root")?);
    let source_root = if request.get("source_root").is_some() {
        Path::new(text(request, "source_root")?)
    } else {
        root
    };
    let spool_dir = Path::new(text(request, "spool_directory")?);
    let dataset = text(request, "dataset_id")?;
    text(request, "collection")?;
    text(request, "embedding_fingerprint")?;
    // The existing frozen descriptor stores dimensions as a string. Preserve
    // that descriptor/fingerprint while using a checked integer for vector IO.
    let dimension = request["embedding"]["vector_size"]
        .as_u64()
        .or_else(|| {
            request["embedding"]["vector_size"]
                .as_str()?
                .parse::<u64>()
                .ok()
        })
        .filter(|v| (1..=65_536).contains(v))
        .ok_or("EMBEDDING_DESCRIPTOR_INVALID")?;
    if !database.is_absolute()
        || !root.is_absolute()
        || !source_root.is_absolute()
        || !spool_dir.is_absolute()
        || dataset.contains(['/', '\\', ':', '\0'])
        || matches!(dataset, "." | "..")
    {
        return Err(INVALID);
    }
    if request["limit"].is_null() && request["allow_unbounded"] != true {
        return Err("INDEX_UNBOUNDED_DISABLED");
    }
    std::fs::create_dir_all(spool_dir).map_err(|_| "INDEX_SPOOL_WRITE_FAILED")?;
    if request["node_preparation"].is_object() {
        crate::document::nodes::Builder::new(&request["node_preparation"])?;
    }
    let mut batch = Batch::acquire(database, dataset)?;
    let started = Instant::now();
    let outcome = (|| {
        recover_storage(&bridge, request).map_err(|_| "INDEX_RECOVERY_INCOMPLETE")?;
        batch.cleanup_spools(spool_dir)?;
        let mut plan = crate::index_sources::plan(
            &json!({"database":database,"root":source_root.join(dataset),"dataset_id":dataset,"limit":request["limit"]}),
        )?;
        if let Some(sources) = request["sources"].as_array() {
            if sources.len() > 50_000 {
                return Err(INVALID);
            }
            // Explicit intake selections still resolve actual file locations in Rust.
            let all = crate::index_sources::plan(
                &json!({"database":database,"root":source_root.join(dataset),"dataset_id":dataset,"limit":Value::Null}),
            )?;
            let files = all["files"].as_array().ok_or(INVALID)?;
            let mut selected = Vec::new();
            for source in sources {
                let found = files.iter().find(|row| {
                    row["db_key"] == source["file_name"]
                        && row["source_path"] == source["source_path"]
                });
                let mut row = found.cloned().unwrap_or_else(||json!({"db_key":source["file_name"],"file_key":source["file_name"],"source_path":source["source_path"],"path":source["source_path"],"error":"INDEX_REGISTRATION_CHANGED"}));
                if source["watch_sha256"].is_string() {
                    row["watch_sha256"] = source["watch_sha256"].clone();
                    row["watch_root"] = source["watch_root"].clone();
                    row["watch_canonical_source"] = source["watch_canonical_source"].clone();
                }
                selected.push(row);
            }
            plan["files"] = json!(selected);
        }
        let files = plan["files"].as_array().ok_or(INVALID)?;
        let mut chunks = 0u64;
        let mut errors = 0u64;
        let mut file_errors = Vec::new();
        let mut attempted = 0u64;
        let mut cache_hits = 0u64;
        let mut embedded = 0u64;
        let mut stop = String::new();
        let mut timings = json!({"cache_sec":0.,"embed_sec":0.,"upsert_sec":0.,"convert_sec":0.});
        let max_attempts = request["max_attempts"].as_u64().unwrap_or(4).clamp(1, 100);
        let every = request["verify_every"].as_u64().unwrap_or(20).max(1);
        for (offset, files_batch) in files.chunks(500).enumerate() {
            let sources = files_batch
                .iter()
                .map(|row| {
                    let mut source =
                        json!({"file_name":row["db_key"],"source_path":row["source_path"]});
                    if row["watch_sha256"].is_string() {
                        source["watch_sha256"] = row["watch_sha256"].clone();
                        source["watch_root"] = row["watch_root"].clone();
                    }
                    source
                })
                .collect::<Vec<_>>();
            let mut dispatch = json!({"database":database,"sources":sources});
            if request["intake_id"].is_string() {
                dispatch["intake_id"] = request["intake_id"].clone();
            } else {
                dispatch["dataset_id"] = json!(dataset);
            }
            let result = crate::index_dispatch::run(&dispatch, |index, attempts| {
                if attempts == 0 {
                    return 0;
                }
                attempted += 1;
                let row = &files_batch[index];
                let name = row["db_key"].as_str().unwrap_or("");
                let registration = match store::registration(&batch.db, dataset, name) {
                    Ok(Some(value)) if value[1] == row["source_path"] => value,
                    Ok(_) => {
                        errors += 1;
                        if file_errors.len() < 20 {
                            file_errors.push(json!({"file_name":name,"error_code":"INDEX_REGISTRATION_CHANGED","retryable":false}));
                        }
                        return 0;
                    }
                    Err(code) => {
                        stop = code.into();
                        return 1;
                    }
                };
                let file_result = (|| -> std::result::Result<Value, Value> {
                    batch.progress(name, "CONVERT").map_err(error)?;
                    if row["error"].as_str().is_some_and(|v| !v.is_empty()) {
                        return Err(error(row["error"].as_str().unwrap()));
                    }
                    let path = row["path"].as_str().ok_or_else(|| error(INVALID))?;
                    let watched = row["watch_sha256"].is_string();
                    if watched
                        && std::fs::canonicalize(path)
                            .ok()
                            .as_deref()
                            .and_then(Path::to_str)
                            != row["watch_canonical_source"].as_str()
                    {
                        return Err(error("SOURCE_CHANGED_DURING_INDEXING"));
                    }
                    let source = fingerprint(path, watched).map_err(error)?;
                    if watched && source["file_hash"] != row["watch_sha256"] {
                        return Err(error("SOURCE_CHANGED_DURING_INDEXING"));
                    }
                    if let Some(expected) = request["expected_source_sha256"].as_str() {
                        if source["file_hash"] != expected {
                            return Err(error("ATTACHMENT_HASH_CHANGED"));
                        }
                    }
                    let mut bytes = [0u8; 16];
                    getrandom::fill(&mut bytes).map_err(|_| error(INVALID))?;
                    let id = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                    batch.cleanup_spools(spool_dir).map_err(error)?;
                    batch.spool(&id).map_err(error)?;
                    let spools = Spools {
                        nodes: spool_dir.join(format!("{id}.nodes")),
                        prepared: spool_dir.join(format!("{id}.prepared.nodes")),
                        points: spool_dir.join(format!("{id}.jsonl")),
                        cache: spool_dir.join(format!("{id}.cache")),
                    };
                    let convert_start = Instant::now();
                    let converted = convert_document(
                        request,
                        &json!({"path":path,"file_name":row["file_key"],"dataset_id":dataset,"node_spool":spools.nodes,
                        "source_sha256":source["file_hash"],"chunking":request["chunking"]}),
                        &bridge,
                    )?;
                    let raw = converted["node_state"] == "les.nodes.raw.v1";
                    let mut count = converted["count"]
                        .as_u64()
                        .filter(|v| raw || *v <= 50_000)
                        .ok_or_else(|| error("INDEX_FILE_CHUNK_LIMIT"))?;
                    if raw {
                        let revision = batch
                            .db
                            .query_row(
                                "SELECT revision_id FROM dataset_memory WHERE dataset_id=?",
                                [dataset],
                                |row| row.get::<_, String>(0),
                            )
                            .unwrap_or_default();
                        count = preparation::prepare(
                            &spools.nodes,
                            &spools.prepared,
                            count,
                            &request["node_preparation"],
                            dataset,
                            row["file_key"].as_str().ok_or_else(|| error(INVALID))?,
                            &revision,
                        )
                        .map_err(error)? as u64;
                    }
                    if count == 0 {
                        return Err(error("NO_SEARCHABLE_TEXT"));
                    }
                    timings["convert_sec"] = json!(
                        timings["convert_sec"].as_f64().unwrap_or(0.)
                            + convert_start.elapsed().as_secs_f64()
                    );
                    if fingerprint(path, watched).map_err(error)? != source {
                        return Err(error("SOURCE_CHANGED_DURING_INDEXING"));
                    }
                    store::stage(&batch.db, dataset, name, &registration, "EMBED")
                        .map_err(error)?;
                    batch.progress(name, "EMBED").map_err(error)?;
                    let cache_start = Instant::now();
                    let cache = if request["cache_enabled"] == true {
                        storage(&bridge,request,"prepare_cache",json!({"dataset_id":dataset,"file_name":row["file_key"],"spool_id":id,
                            "embedding_fingerprint":request["embedding_fingerprint"],"dimension":dimension,"dense":request["vector_names"]["dense"]})).is_ok()
                    } else {
                        false
                    };
                    timings["cache_sec"] = json!(
                        timings["cache_sec"].as_f64().unwrap_or(0.)
                            + cache_start.elapsed().as_secs_f64()
                    );
                    let nodes = RefCell::new(
                        Nodes::open(if raw { &spools.prepared } else { &spools.nodes })
                            .map_err(error)?,
                    );
                    let callback_error = RefCell::new(None);
                    let prepared=crate::index_points::run(&json!({"spool":spools.points,"count":count,"batch_size":request["embed_batch"],
                        "dimension":dimension,"vector_names":request["vector_names"],"dataset_id":dataset,
                        "file_name":row["file_key"],"source_sha256":source["file_hash"],"embedding":request["embedding"],
                        "embedding_fingerprint":request["embedding_fingerprint"],"cache":if cache{json!(spools.cache)}else{Value::Null}}),|op,value|{
                            if op==0{nodes.borrow_mut().take(value)}else{match exchange(&bridge,1,value){Ok(value)=>Ok(value),Err(value)=>{*callback_error.borrow_mut()=Some(value);Err("INDEX_POINTS_ADAPTER_FAILED")}}}
                        }).map_err(|code|callback_error.borrow_mut().take().unwrap_or_else(||error(code)))?;
                    nodes.borrow_mut().finished().map_err(error)?;
                    drop(nodes); // Close the immutable prepared spool before cleanup.
                    store::stage(&batch.db, dataset, name, &registration, "UPSERT")
                        .map_err(error)?;
                    batch.progress(name, "UPSERT").map_err(error)?;
                    let ids = prepared["ids"].as_array().ok_or_else(|| error(INVALID))?;
                    let publish_start = Instant::now();
                    let published = storage(
                        &bridge,
                        request,
                        "publish_file",
                        json!({"dataset_id":dataset,"doc_name":row["file_key"],"db_file_name":name,"spool_id":id,
                        "registration":registration,"fingerprint":source,"route":converted["route"],"ids":ids,"batch_size":request["upsert_batch"],
                        "verify_count":(offset*500+index+1) as u64%every==0||offset*500+index+1==files.len()}),
                    );
                    if let Err(failure) = published {
                        if recover_storage(&bridge, request).is_err() {
                            stop = "INDEX_RECOVERY_INCOMPLETE".into();
                            return Err(failure);
                        }
                        let committed_name = if registration[1] == "" {
                            row["file_key"].as_str().unwrap_or(name)
                        } else {
                            name
                        };
                        if !store::committed(
                            &batch.db,
                            dataset,
                            committed_name,
                            &registration[0],
                            source["file_hash"].as_str().unwrap_or(""),
                            ids.len(),
                        )
                        .map_err(error)?
                        {
                            return Err(failure);
                        }
                    } else if published.as_ref().unwrap()["status"] != "completed"
                        || published.as_ref().unwrap()["points"].as_u64() != Some(ids.len() as u64)
                    {
                        stop = "INDEX_PUBLICATION_RESULT_INVALID".into();
                        return Err(error(&stop));
                    }
                    let committed_name = if registration[1] == "" {
                        row["file_key"].as_str().unwrap_or(name)
                    } else {
                        name
                    };
                    if !store::committed(
                        &batch.db,
                        dataset,
                        committed_name,
                        &registration[0],
                        source["file_hash"].as_str().unwrap_or(""),
                        ids.len(),
                    )
                    .map_err(error)?
                    {
                        stop = "INDEX_PUBLICATION_RESULT_INVALID".into();
                        return Err(error(&stop));
                    }
                    timings["upsert_sec"] = json!(
                        timings["upsert_sec"].as_f64().unwrap_or(0.)
                            + publish_start.elapsed().as_secs_f64()
                    );
                    Ok(prepared)
                })();
                match file_result {
                    Ok(result) => {
                        chunks += result["ids"].as_array().map_or(0, |v| v.len()) as u64;
                        cache_hits += result["cache_hits"].as_u64().unwrap_or(0);
                        embedded += result["embedded"].as_u64().unwrap_or(0);
                        timings["embed_sec"] = json!(
                            timings["embed_sec"].as_f64().unwrap_or(0.)
                                + result["embed_sec"].as_f64().unwrap_or(0.)
                        );
                    }
                    Err(failure) => {
                        let (code, retry) = policy::classify(&failure, attempts, max_attempts);
                        if file_errors.len() < 20 {
                            file_errors.push(json!({"file_name":name,"error_code":code,"retryable":retry>0.,"error_type":failure["error_type"],"message":failure["message"]}));
                        }
                        if code != "UNSUPPORTED_INDEXING_SOURCE" {
                            errors += 1
                        }
                        if let Err(code) = store::failure(
                            &batch.db,
                            dataset,
                            name,
                            &registration,
                            &code,
                            failure["message"].as_str().unwrap_or(&code),
                            retry,
                        ) {
                            stop = code.into();
                        }
                        eprintln!(
                            "{}",
                            json!({"event":"index_batch_file_failed","batch_id":batch.id,"code":code,"attempt":attempts,"retryable":retry>0.})
                        );
                    }
                }
                if stop.is_empty() {
                    0
                } else {
                    1
                }
            })?;
            if result["interrupted"] == true {
                break;
            }
        }
        batch.cleanup_spools(spool_dir)?;
        let remaining = crate::index_sources::plan(
            &json!({"database":database,"dataset_id":dataset,"count_only":true}),
        )?["remaining_pending"]
            .clone();
        let result = json!({"status":if stop.is_empty(){"completed"}else{"failed"},"owner":"rust.index-batch","batch_id":batch.id,
            "chunks":chunks,"files_parsed":attempted,"files_skipped":plan["total_files"].as_u64().unwrap_or(0).saturating_sub(plan["matched_internal"].as_u64().unwrap_or(0)),
            "remaining_pending":remaining,"errors":errors,"file_errors":file_errors,"omitted_file_errors":errors.saturating_sub(file_errors.len() as u64),"embedding_cache_hits":cache_hits,"embedded_chunks":embedded,"elapsed_sec":started.elapsed().as_secs_f64(),"timings":timings,"error":if stop.is_empty(){Value::Null}else{json!(stop)}});
        Ok((result, stop))
    })();
    match outcome {
        Ok((result, stop)) => {
            batch.finish(&result, &stop)?;
            Ok(result)
        }
        Err(code) => {
            eprintln!(
                "{}",
                json!({"event":"index_batch_failed","batch_id":batch.id,"code":code})
            );
            let result = json!({"status":"failed","owner":"rust.index-batch","batch_id":batch.id,"error":code});
            batch.finish(&result, code)?;
            Ok(result)
        }
    }
}
