//! One native format dispatch: source -> normalized rows -> immutable Parquet -> raw nodes.
use super::{presentation, spreadsheet, Result};
use crate::tables;
use serde_json::{json, Value};
use std::{
    cell::RefCell,
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};
const INVALID: &str = "DOCUMENT_DISPATCH_INVALID";

fn text<'a>(request: &'a Value, key: &str) -> Result<&'a str> {
    request[key].as_str().ok_or(INVALID)
}
fn integer(request: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<usize> {
    request
        .get(key)
        .map_or(Some(default), Value::as_u64)
        .filter(|v| (min..=max).contains(v))
        .map(|v| v as usize)
        .ok_or(INVALID)
}
fn typing(table: bool, role: &str) -> Value {
    json!({"file_kind":if table{"table"}else{"document"},"content_layers":[if table{"tables"}else{"text"}],
        "content_layer_labels":[if table{"Таблицы"}else{"Текст"}],"document_role":role,"navigation_terms":[],
        "source_granularity":if table{"table_row_or_cell"}else{"chunk"},"typed_by":"rust.format-dispatch.v1"})
}
fn source_nodes(
    request: &Value,
    body: &str,
    metadata: &Value,
    payload_type: &str,
    pdf: bool,
) -> Result<Vec<Value>> {
    let offsets = body
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(body.len()))
        .collect::<Vec<_>>();
    let nodes = RefCell::new(Vec::new());
    let chunking = &request["chunking"];
    let size = if pdf {
        integer(request, "pdf_page_size", 1800, 800, 1_000_000)? as u64
    } else {
        chunking["chunk_size"].as_u64().ok_or(INVALID)?
    };
    let overlap = if pdf {
        integer(request, "pdf_page_overlap", 150, 0, size - 1)? as u64
    } else {
        chunking["chunk_overlap"].as_u64().ok_or(INVALID)?
    };
    super::super::source_nodes::run(
        &json!({"schema":"les.document-source-nodes.v1","kind":if pdf {"pdf"} else {"markdown"},"ocr":metadata["ocr"],
        "dataset_id":request["dataset_id"],"file_key":request["file_key"],"size":size,"overlap":overlap,
        "unit":chunking["unit"],"tokenizer":chunking["tokenizer"],"minimum":request["minimum"],"final_minimum":request["final_minimum"],
        "payload_type":payload_type,"route_metadata":metadata}),
        |op, value| match op {
            0 => {
                let start = value["offset"].as_u64().ok_or(INVALID)? as usize;
                let length = value["limit"].as_u64().ok_or(INVALID)? as usize;
                let end = (start + length).min(offsets.len() - 1);
                if start > end {
                    return Err(INVALID);
                }
                Ok(
                    json!({"text":&body[offsets[start]..offsets[end]],"complete":end==offsets.len()-1}),
                )
            }
            1 => {
                nodes
                    .borrow_mut()
                    .extend(value["nodes"].as_array().ok_or(INVALID)?.iter().cloned());
                Ok(json!({"ok":true}))
            }
            _ => Err(INVALID),
        },
    )?;
    Ok(nodes.into_inner())
}
fn markdown(
    request: &Value,
    body: &str,
    metadata: &Value,
    payload_type: &str,
) -> Result<Vec<Value>> {
    source_nodes(request, body, metadata, payload_type, false)
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn artifact(
    request: &Value,
    path: &Path,
    rows: &Value,
    source_hash: &str,
) -> Result<(String, String)> {
    let root = Path::new(text(request, "data_dir")?);
    let key = Path::new(text(request, "file_key")?);
    if !root.is_absolute()
        || key.as_os_str().is_empty()
        || !key.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(INVALID);
    }
    fs::create_dir_all(root).map_err(|_| "PARQUET_WRITE_FAILED")?;
    let canonical = root.canonicalize().map_err(|_| INVALID)?;
    let relative = Path::new("_parquet/.les-versions").join(crate::sha256_hex(
        key.to_str().ok_or(INVALID)?.replace('\\', "/").as_bytes(),
    ));
    let folder = root.join(&relative);
    // Check each existing ancestor before creating anything below it.
    let mut ancestor = folder.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or(INVALID)?;
    }
    if !ancestor
        .canonicalize()
        .map_err(|_| INVALID)?
        .starts_with(&canonical)
    {
        return Err("PARQUET_PATH_INVALID");
    }
    fs::create_dir_all(&folder).map_err(|_| "PARQUET_WRITE_FAILED")?;
    if !folder
        .canonicalize()
        .map_err(|_| INVALID)?
        .starts_with(&canonical)
    {
        return Err("PARQUET_PATH_INVALID");
    }
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| INVALID)?;
    let id = random
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    let temporary = Temporary(folder.join(format!("preparing-{id}.parquet")));
    let written = tables::execute(&json!({"action":"write","path":temporary.0,"rows":rows}))?;
    let checksum = written["sha256"].as_str().ok_or(INVALID)?;
    let name = format!("{source_hash}-{checksum}.parquet");
    let destination = folder.join(&name);
    if destination.exists() {
        let existing = tables::execute(
            &json!({"action":"read","path":destination,"limit":1,"source_sha256":checksum}),
        )?;
        if existing["source_sha256"] != checksum {
            return Err("PARQUET_SOURCE_CHANGED");
        }
    } else {
        fs::rename(&temporary.0, &destination).map_err(|_| "PARQUET_WRITE_FAILED")?;
    }
    let _ = path;
    Ok((
        relative.join(name).to_string_lossy().replace('\\', "/"),
        checksum.into(),
    ))
}
fn projection(file: &str, prepared: &Value, parquet: &str) -> String {
    let mut lines = vec![
        format!("Табличный файл: {file}"),
        "Тип: table_navigation_projection".into(),
        format!(
            "Строк нормализовано: {}",
            prepared["rows"].as_array().map_or(0, Vec::len)
        ),
        format!("Листов/таблиц: {}", prepared["sheets"]),
        format!("Parquet: {parquet}"),
        "Назначение: навигация по таблице; точные строки и расчеты читать из источника.".into(),
    ];
    for (key, label) in [
        ("section", "Разделы"),
        ("code", "Коды/обозначения"),
        ("name", "Наименования"),
        ("unit", "Единицы"),
    ] {
        let mut seen = HashSet::new();
        let values = prepared["chunks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["metadata"][key].as_str())
            .filter(|s| !s.trim().is_empty() && seen.insert(*s))
            .take(20)
            .map(|s| s.chars().take(160).collect::<String>())
            .collect::<Vec<_>>();
        if !values.is_empty() {
            lines.push(format!("{label}: {}", values.join("; ")));
        }
    }
    lines.join("\n")
}
pub(super) fn prepare(
    request: &Value,
    bytes: &[u8],
    path: &Path,
    suffix: &str,
    source_hash: &str,
    content: Option<&Value>,
) -> Result<Value> {
    let dataset = text(request, "dataset_id")?;
    let file = text(request, "file_key")?;
    if dataset.is_empty()
        || !Path::new(file)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(INVALID);
    }
    let minimum = integer(request, "minimum", 100, 0, 1_000_000)?;
    let final_minimum = integer(request, "final_minimum", 20, 0, 1_000_000)?;
    let mut request = request.clone();
    request["minimum"] = json!(minimum);
    request["final_minimum"] = json!(final_minimum);
    let mut route = json!({"domain":"DOCUMENT","dataset_name":"DOCUMENT_Index","doc_type":"OTHER","content_type":"text","complexity":"simple","pipeline":"markdown","metadata":{}});
    let mut nodes;
    if let Some(content) = content {
        let pdf = content["kind"] == "pdf";
        let mut metadata = content["metadata"].as_object().cloned().unwrap_or_default();
        metadata.insert("domain".into(), json!("DOCUMENT"));
        metadata.insert("doc_type".into(), json!("OTHER"));
        metadata.insert("content_type".into(), json!("text"));
        route["metadata"] = json!(metadata);
        nodes = if let Some(cards) = content["engineering_cards"].as_array() {
            let mut collected = Vec::new();
            for card in cards {
                let mut object_request = request.clone();
                object_request["file_key"] = json!(format!(
                    "{file}/{}",
                    crate::sha256_hex(card["model_locator"]["object_id"].to_string().as_bytes())
                ));
                object_request["minimum"] = json!(0);
                object_request["final_minimum"] = json!(0);
                let mut metadata = route["metadata"].clone();
                metadata["model_locator"] = card["model_locator"].clone();
                metadata["parent_id"] = json!(crate::sha256_hex(
                    format!(
                        "{dataset}:{file}:{source_hash}:{}",
                        card["model_locator"]["object_id"]
                    )
                    .as_bytes()
                ));
                metadata["reading_representation"] = json!("engineering_object");
                collected.extend(source_nodes(
                    &object_request,
                    card["text"].as_str().ok_or(INVALID)?,
                    &metadata,
                    "engineering_object",
                    false,
                )?);
            }
            collected
        } else {
            source_nodes(
                &request,
                content["text"].as_str().ok_or(INVALID)?,
                &route["metadata"],
                if pdf { "pdf_page_text" } else { "markdown" },
                pdf,
            )?
        };
        route["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("pdf_page_views");
    } else if suffix == "pptx" {
        let content = presentation::read(bytes)?;
        route["metadata"] = json!({"domain":"DOCUMENT","doc_type":"OTHER","content_type":"text","slide_count":content["slides"],"format_owner":"rust.format-dispatch.v1"});
        nodes = markdown(
            &request,
            content["text"].as_str().ok_or(INVALID)?,
            &route["metadata"],
            "markdown",
        )?;
    } else {
        let mut read = request.clone();
        read["view"] = json!("tables");
        read["read_rows"] = json!(20_000);
        let sheets = spreadsheet::read(bytes, suffix, path, &read)?;
        let prepared = tables::execute(
            &json!({"action":"prepare","sheets":sheets["sheets"],"dataset_id":dataset,
            "stem":path.file_stem().and_then(|s|s.to_str()).ok_or(INVALID)?,"source_file":path.file_name().and_then(|s|s.to_str()).ok_or(INVALID)?,
            "workbook":suffix!="csv","doc_type":request["doc_type"]}),
        )?;
        let kind = prepared["doc_type"].as_str().ok_or(INVALID)?;
        route = json!({"domain":"TABLE","dataset_name":"TABLE_Index","doc_type":kind,"content_type":"table","complexity":"simple","pipeline":"parquet",
            "metadata":{"domain":"TABLE","dataset_name":"TABLE_Index","doc_type":kind,"content_type":"table","complexity":"simple","pipeline":"parquet",
                "sheet_count":prepared["sheets"],"row_count_hint":prepared["rows"].as_array().map_or(0,Vec::len),"format_owner":"rust.format-dispatch.v1"}});
        let chunks = prepared["chunks"].as_array().ok_or(INVALID)?;
        nodes = Vec::new();
        if !prepared["rows"].as_array().ok_or(INVALID)?.is_empty() {
            let (parquet, checksum) = artifact(&request, path, &prepared["rows"], source_hash)?;
            let mut common = route["metadata"].clone();
            common["parquet_path"] = json!(parquet);
            common["parquet_sha256"] = json!(checksum);
            common["parquet_source_sha256"] = json!(source_hash);
            common["table_kind"] = json!("table");
            if chunks.len() > integer(&request, "table_row_limit", 600, 1, 200_000)? {
                let mut payload = common.clone();
                payload["type"] = json!("table_navigation_projection");
                payload["table_rows"] = json!(prepared["rows"].as_array().unwrap().len());
                payload["table_sheets"] = prepared["sheets"].clone();
                nodes.push(json!({"text":projection(file,&prepared,&parquet),"doc_id":super::super::source_nodes::uuid5(&format!("{dataset}:{}:table_projection",path.display())),"payload":payload}));
            } else {
                for (index, chunk) in chunks.iter().enumerate() {
                    let body = chunk["text"].as_str().ok_or(INVALID)?;
                    if body.chars().count() < minimum {
                        continue;
                    }
                    let mut payload = common.as_object().unwrap().clone();
                    payload.extend(chunk["metadata"].as_object().ok_or(INVALID)?.clone());
                    payload.insert("table_row".into(), json!(index));
                    nodes.push(json!({"text":body,"doc_id":super::super::source_nodes::uuid5(&format!("{dataset}:{}:{index}",path.display())),"payload":payload}));
                }
            }
            if nodes.is_empty() {
                let mut read = request.clone();
                read["view"] = json!("markdown");
                let markdown_content = spreadsheet::read(bytes, suffix, path, &read)?;
                nodes = markdown(
                    &request,
                    markdown_content["text"].as_str().ok_or(INVALID)?,
                    &common,
                    "markdown",
                )?;
            }
        }
    }
    if nodes.is_empty() {
        return Err("NO_SEARCHABLE_TEXT");
    }
    let table = matches!(suffix, "xlsx" | "xlsm" | "xls" | "csv");
    let role = if route["doc_type"] == "SPEC" {
        "спецификация"
    } else {
        "документ"
    };
    for node in &mut nodes {
        node["_les_file_typing"] = typing(table, role);
        node["payload"]["format_owner"] = json!("rust.format-dispatch.v1");
    }
    let count = nodes.len();
    if let Some(spool) = request["node_spool"].as_str() {
        let spool = Path::new(spool);
        if !spool.is_absolute() || spool.extension().is_none_or(|v| v != "nodes") {
            return Err(INVALID);
        }
        let mut stream = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(spool)
            .map_err(|_| "INDEX_NODE_SPOOL_FAILED")?;
        let written = (|| -> Result<()> {
            for node in &nodes {
                serde_json::to_writer(&mut stream, node).map_err(|_| "INDEX_NODE_SPOOL_FAILED")?;
                stream
                    .write_all(b"\n")
                    .map_err(|_| "INDEX_NODE_SPOOL_FAILED")?;
            }
            stream.sync_all().map_err(|_| "INDEX_NODE_SPOOL_FAILED")
        })();
        if written.is_err() {
            drop(stream);
            let _ = fs::remove_file(spool);
        }
        written?;
        return Ok(
            json!({"text":"","route":route,"count":count,"node_state":"les.nodes.raw.v1","owner":"rust.format-dispatch","status":"completed"}),
        );
    }
    Ok(
        json!({"text":"","route":route,"nodes":nodes,"count":count,"owner":"rust.format-dispatch","status":"completed"}),
    )
}
