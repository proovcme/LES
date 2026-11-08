//! Publish the prepared points of one file under the collection writer lease.
use super::{collection_name, sections, spool_path, valid_id, write, Store};
use crate::{error, json_response, Context};
use axum::{http::StatusCode, response::Response};
use les_core::index_publication::{check_registration, check_source, commit, recover, Writer};
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Component, Path},
    sync::Arc,
};

const INVALID: &str = "INDEX_PUBLICATION_INVALID";
const STORAGE: &str = "INDEX_CATALOG_WRITE_FAILED";
const RECOVERY: &str = "INDEX_RECOVERY_INCOMPLETE";
const MAX_LINE: u64 = 8 * 1024 * 1024;
const MAX_BATCH_BYTES: usize = 16 * 1024 * 1024;
const LEXICAL_FIELDS: &[&str] = &[
    "dataset_id",
    "doc_id",
    "text",
    "content_hash",
    "chunk_ord",
    "section_heading",
    "parent_id",
    "parent_ord",
    "child_ord",
    "parent_heading",
    "context_before",
    "context_after",
    "context_kind",
    "parquet_path",
    "parquet_sha256",
    "parquet_source_sha256",
];

fn lexical_row(point: &Value, expected_id: &Value, body: &Value) -> Result<Value, &'static str> {
    let payload = &point["payload"];
    if point["id"] != *expected_id
        || payload["dataset_id"] != body["dataset_id"]
        || payload["file_name"] != body["doc_name"]
        || (body["action"] == "publish_file"
            && (!point["vector"].is_object() && !point["vector"].is_array()
                || payload["source_sha256"] != body["fingerprint"]["file_hash"]))
        || ["text", "content_hash"].iter().any(|key| {
            !payload[*key]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
        })
    {
        return Err(INVALID);
    }
    let mut row: Map<String, Value> = LEXICAL_FIELDS
        .iter()
        .map(|&key| (key.into(), payload[key].clone()))
        .collect();
    row.insert("point_id".into(), expected_id.clone());
    row.insert("doc_name".into(), payload["file_name"].clone());
    Ok(row.into())
}

fn read_point(reader: &mut BufReader<File>, line: &mut Vec<u8>) -> Result<Value, &'static str> {
    line.clear();
    if reader
        .take(MAX_LINE + 1)
        .read_until(b'\n', line)
        .map_err(|_| INVALID)? as u64
        > MAX_LINE
        || !line.ends_with(b"\n")
    {
        return Err(INVALID);
    }
    serde_json::from_slice(line).map_err(|_| INVALID)
}

async fn stage(
    context: &Context,
    store: &Store,
    collection: &str,
    body: &Value,
    ids: &[Value],
    mut reader: BufReader<File>,
) -> Result<Vec<Value>, &'static str> {
    let mut lexical = Vec::with_capacity(ids.len());
    let mut offset = 0;
    let batch_size = body["batch_size"].as_u64().unwrap_or(100).clamp(1, 512) as usize;
    let mut line = Vec::new();
    while offset < ids.len() {
        let mut bytes = 0;
        let mut points = Vec::new();
        for id in ids[offset..].iter().take(batch_size) {
            let point = read_point(&mut reader, &mut line)?;
            lexical.push(lexical_row(&point, id, body)?);
            points.push(point);
            bytes += line.len();
            if bytes >= MAX_BATCH_BYTES {
                break;
            }
        }
        offset += points.len();
        write(
            context,
            store,
            collection,
            reqwest::Method::PUT,
            "",
            json!({"points":points}),
            None,
        )
        .await?;
    }
    if !reader.fill_buf().map_err(|_| INVALID)?.is_empty() {
        return Err(INVALID);
    }
    Ok(lexical)
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

async fn publish(context: &Context, store: &Store, mut body: Value) -> Result<usize, &'static str> {
    let collection = collection_name(&body).ok_or(INVALID)?.to_owned();
    let spool = spool_path(context, &body, "jsonl").ok_or(INVALID)?;
    let dataset = body["dataset_id"]
        .as_str()
        .filter(|id| relative(id) && !id.contains(['/', '\\']))
        .ok_or(INVALID)?
        .to_owned();
    let name = body["doc_name"]
        .as_str()
        .filter(|name| relative(name))
        .ok_or(INVALID)?
        .to_owned();
    let database_name = body["db_file_name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .ok_or(INVALID)?
        .to_owned();
    let doc = body["registration"][0]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(INVALID)?
        .to_owned();
    let ids = body["ids"].take();
    let ids = ids
        .as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= 50_000 && ids.iter().all(valid_id))
        .ok_or(INVALID)?;
    let content = context.state_root.join("storage/datasets");
    let source = body["registration"][1].as_str().ok_or(INVALID)?;
    let source = if source.is_empty() {
        content.join(&dataset).join(&name)
    } else {
        source.into()
    };
    if !source.is_absolute() {
        return Err(INVALID);
    }
    body["database"] = json!(context.database);
    let fingerprint = &body["fingerprint"];
    if !fingerprint["file_hash"]
        .as_str()
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || fingerprint["file_mtime"].as_f64().is_none()
        || fingerprint["file_size"].as_u64().is_none()
        || [
            "domain",
            "dataset_name",
            "doc_type",
            "content_type",
            "complexity",
            "pipeline",
        ]
        .iter()
        .any(|key| !body["route"][*key].is_string())
    {
        return Err(INVALID);
    }
    check_registration(&body)?;
    // Reading an external disk must not hold the collection writer lease.
    check_source(&source, fingerprint)?;
    let writer = Writer::acquire(&store.content_root(context), &collection)?;
    check_registration(&body)?;
    let reader = BufReader::new(File::open(spool).map_err(|_| INVALID)?);
    writer.journal(
        "begin",
        json!({"dataset":dataset,"file_name":name,"db_file_name":database_name,
        "document_id":doc,"ids":ids}),
    )?;
    let staged = async {
        let rows = stage(context, store, &collection, &body, ids, reader).await?;
        check_registration(&body)?;
        Ok::<_, &'static str>(rows)
    }
    .await;
    let rows = match staged {
        Ok(rows) => rows,
        Err(code) => {
            // Keep the staging decision if the server cannot confirm rollback.
            write(
                context,
                store,
                &collection,
                reqwest::Method::POST,
                "delete",
                json!({"points":ids}),
                None,
            )
            .await?;
            writer.journal("finish", json!({}))?;
            return Err(code);
        }
    };
    // Originals remain under the application source root; immutable derivatives
    // belong to the storage owner (Edge may have a different content directory).
    les_core::tables::artifacts::verify(
        &json!({"dataset_id":dataset,"rows":rows}),
        &store.content_root(context),
    )?;
    writer.journal("commit", json!({}))?;
    retire(context, store, &collection, &body, ids).await?;
    if body["verify_count"] == true {
        write(
            context,
            store,
            &collection,
            reqwest::Method::POST,
            "count",
            json!({"filter":{"must":scope(&body)},"exact":true}),
            Some(ids.len()),
        )
        .await?;
    }
    let count = ids.len();
    body["rows"] = rows.into();
    commit(&body, count)?;
    writer.journal("finish", json!({}))?;
    Ok(count)
}

fn scope(body: &Value) -> Value {
    json!([{"key":"dataset_id","match":{"value":body["dataset_id"]}},
        {"key":"file_name","match":{"value":body["doc_name"]}}])
}

async fn retire(
    context: &Context,
    store: &Store,
    collection: &str,
    body: &Value,
    ids: &[Value],
) -> Result<(), &'static str> {
    write(
        context,
        store,
        collection,
        reqwest::Method::POST,
        "delete",
        json!({"filter":{"must":scope(body),"must_not":[{"has_id":ids}]}}),
        None,
    )
    .await
}

pub(super) async fn recover_file(
    context: &Context,
    store: &Store,
    body: &Value,
) -> Result<Value, &'static str> {
    let collection = collection_name(body).ok_or(INVALID)?;
    let writer = Writer::acquire(&store.content_root(context), collection)?;
    let decision = writer.journal("read", json!({}))?;
    let phase = decision["phase"].as_str().unwrap_or("idle");
    if matches!(phase, "idle" | "sparse_refresh" | "sparse_dirty") {
        drop(writer);
        store.refresh_postings().await?;
        return Ok(json!({"status":"idle"}));
    }
    if !decision["document_id"].is_string() {
        return Ok(json!({"status":"legacy"}));
    }
    let ids = decision["ids"]
        .as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= 50_000 && ids.iter().all(valid_id))
        .ok_or(RECOVERY)?;
    let mut request = json!({"action":"recover_file","phase":phase,"database":context.database,
        "collection":collection,"dataset_id":decision["dataset"],"doc_name":decision["file_name"],
        "document_id":decision["document_id"],"db_file_name":decision["db_file_name"]});
    les_core::index_publication::check_recovery_registration(&request)?;
    match phase {
        "staging" => {
            write(
                context,
                store,
                collection,
                reqwest::Method::POST,
                "delete",
                json!({"points":ids}),
                None,
            )
            .await?
        }
        "committing" => {
            let mut rows = Vec::with_capacity(ids.len());
            for ids in ids.chunks(32) {
                let found = sections::request(
                    context,
                    store,
                    collection,
                    false,
                    json!({"ids":ids,"with_payload":true,"with_vector":false}),
                )
                .await
                .map_err(|_| RECOVERY)?;
                let points: HashMap<_, _> = found
                    .as_array()
                    .ok_or(RECOVERY)?
                    .iter()
                    .filter_map(|point| Some((point["id"].as_str()?, point)))
                    .collect();
                for id in ids {
                    let point = points.get(id.as_str().ok_or(RECOVERY)?).ok_or(RECOVERY)?;
                    rows.push(lexical_row(point, id, &request).map_err(|_| RECOVERY)?);
                }
            }
            request["rows"] = rows.into();
            retire(context, store, collection, &request, ids).await?;
        }
        _ => return Err(RECOVERY),
    }
    recover(&request)?;
    writer.journal("finish", json!({}))?;
    drop(writer);
    store.refresh_postings().await?;
    eprintln!(
        "{}",
        json!({"event":"index_file_recovered","dataset_id":request["dataset_id"],
        "file":request["doc_name"],"phase":phase,"reason":"INDEX_INTERRUPTED"})
    );
    Ok(json!({"status":"recovered"}))
}

pub(super) async fn handle(context: Arc<Context>, body: Value) -> Response {
    // The client's lifetime must not interrupt a Qdrant mutation in flight.
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let store = context
            .index_store
            .as_ref()
            .expect("authorized index store");
        runtime.block_on(respond(&context, store, body))
    })
    .await
    .unwrap_or_else(|_| error(StatusCode::BAD_GATEWAY, STORAGE))
}

async fn respond(context: &Context, store: &Store, body: Value) -> Response {
    if body["action"] == "recover_file" {
        return match recover_file(context, store, &body).await {
            Ok(result) => json_response(StatusCode::OK, result, "rust.index_publication"),
            Err(code) => {
                eprintln!(
                    "{}",
                    json!({"event":"index_recovery_stopped","reason":code})
                );
                error(StatusCode::BAD_GATEWAY, code)
            }
        };
    }
    let dataset = body["dataset_id"].clone();
    let file = body["doc_name"].clone();
    match publish(context, store, body).await {
        Ok(points) => {
            if let Err(code) = store.refresh_postings().await {
                eprintln!("{}", json!({"event":"index_lexical_stopped","reason":code}));
                return error(StatusCode::BAD_GATEWAY, code);
            }
            eprintln!(
                "{}",
                json!({"event":"index_file_committed","dataset_id":dataset,"file":file,"points":points})
            );
            json_response(
                StatusCode::OK,
                json!({"status":"completed","points":points}),
                "rust.index_publication",
            )
        }
        Err(code) => {
            eprintln!(
                "{}",
                json!({"event":"index_file_stopped","dataset_id":dataset,"file":file,"reason":code})
            );
            let status = match code {
                INVALID => StatusCode::UNPROCESSABLE_ENTITY,
                "INDEX_REGISTRATION_CHANGED"
                | "INDEX_SOURCE_ALIAS_CONFLICT"
                | "SOURCE_CHANGED_DURING_INDEXING"
                | "INDEX_UPDATE_BUSY" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_GATEWAY,
            };
            error(status, code)
        }
    }
}
