//! Assemble one file's point spool before any index mutation.
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    fmt::Write as _,
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

type Result<T> = std::result::Result<T, &'static str>;
const INVALID: &str = "INDEX_POINTS_INVALID";
const WRITE_FAILED: &str = "INDEX_SPOOL_WRITE_FAILED";
const FILE_CHUNK_LIMIT: usize = 50_000;

const EMBEDDING_FIELDS: &[&str] = &[
    "backend",
    "model_id",
    "profile",
    "coreml_model",
    "coreml_seq_len",
    "coreml_compute_units",
    "coreml_fallback",
];

fn uuid() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "INDEX_POINT_ID_UNAVAILABLE")?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        let _ = write!(id, "{byte:02x}");
    }
    Ok(id)
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or(INVALID)
}

fn positive(value: &Value, key: &str) -> Result<usize> {
    value[key]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or(INVALID)
}

fn valid_vector(vector: &Value, dimension: usize) -> bool {
    vector.as_array().is_some_and(|items| {
        items.len() == dimension
            && items.iter().all(|item| {
                item.as_f64()
                    .is_some_and(|value| (value as f32).is_finite())
            })
    })
}

pub(super) fn run(request: &Value, bridge: impl Fn(u8, &Value) -> Result<Value>) -> Result<Value> {
    let path = Path::new(text(request, "spool")?);
    if !path.is_absolute() {
        return Err(INVALID);
    }
    let count = positive(request, "count")?;
    let dimension = positive(request, "dimension")?;
    let batch_size = positive(request, "batch_size")?.min(512);
    let vector_names = if request["vector_names"].is_null() {
        None
    } else {
        let dense = text(&request["vector_names"], "dense")?;
        let sparse = text(&request["vector_names"], "sparse")?;
        if dense == sparse {
            return Err(INVALID);
        }
        Some((dense, sparse))
    };
    let mut common = Map::new();
    for key in [
        "dataset_id",
        "file_name",
        "source_sha256",
        "embedding_fingerprint",
    ] {
        common.insert(key.into(), text(request, key)?.into());
    }
    for field in EMBEDDING_FIELDS {
        let value = request["embedding"][*field].as_str().unwrap_or("");
        common.insert(format!("embedding_{field}"), value.into());
    }
    let (cache, cache_read_failed) = match request["cache"].as_str() {
        Some(path) => match crate::index_cache::load(Path::new(path), dimension) {
            Ok(cache) => (cache, false),
            Err(_) => (HashMap::new(), true),
        },
        None => (HashMap::new(), false),
    };
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| WRITE_FAILED)?;
    let mut writer = BufWriter::new(file);
    let result = write_points(
        &mut writer,
        count,
        batch_size,
        dimension,
        vector_names,
        &common,
        &cache,
        bridge,
    );
    drop(writer);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result.map(|mut result| {
        result["cache_read_failed"] = cache_read_failed.into();
        result
    })
}

fn write_points(
    writer: &mut impl Write,
    count: usize,
    batch_size: usize,
    dimension: usize,
    names: Option<(&str, &str)>,
    common: &Map<String, Value>,
    cache: &HashMap<String, Vec<f32>>,
    bridge: impl Fn(u8, &Value) -> Result<Value>,
) -> Result<Value> {
    let mut ids = Vec::with_capacity(count.min(FILE_CHUNK_LIMIT));
    let mut offset = 0;
    let mut skipped = 0;
    let mut reused = 0;
    let mut embedded = 0;
    let mut embed_sec = 0.0;
    let mut line = Vec::new();
    while offset < count {
        let limit = batch_size.min(count - offset);
        let response = bridge(0, &json!({"offset":offset,"limit":limit}))?;
        let Value::Array(nodes) = response else {
            return Err(INVALID);
        };
        if nodes.is_empty() || nodes.len() > limit {
            return Err(INVALID);
        }
        offset += nodes.len();
        let mut selected = Vec::with_capacity(nodes.len());
        for node in nodes {
            let content = node["text"].as_str().ok_or(INVALID)?;
            let sparse = if names.is_some() {
                let Some(sparse) = crate::legacy_sparse::vector(content) else {
                    skipped += 1;
                    continue;
                };
                sparse
            } else {
                Value::Null
            };
            selected.push((node, sparse));
        }
        if selected.len() > FILE_CHUNK_LIMIT - ids.len() {
            return Err("INDEX_FILE_CHUNK_LIMIT");
        }
        let mut vectors = Vec::with_capacity(selected.len());
        let mut missing = Vec::new();
        let mut texts = Vec::new();
        for (index, (node, _)) in selected.iter().enumerate() {
            let content = text(node, "text")?;
            let hash = text(&node["payload"], "content_hash")?;
            if let Some(cached) = cache.get(hash) {
                reused += 1;
                vectors.push(json!(cached));
            } else {
                vectors.push(Value::Null);
                missing.push(index);
                texts.push(content);
            }
        }
        if !missing.is_empty() {
            let started = Instant::now();
            let response = bridge(1, &json!(texts))?;
            embed_sec += started.elapsed().as_secs_f64();
            let Value::Array(fresh) = response else {
                return Err("EMBEDDING_VECTOR_INVALID");
            };
            if fresh.len() != missing.len() {
                return Err("EMBEDDING_COUNT_MISMATCH");
            }
            for (index, vector) in missing.into_iter().zip(fresh) {
                if !valid_vector(&vector, dimension) {
                    return Err("EMBEDDING_VECTOR_INVALID");
                }
                vectors[index] = vector;
                embedded += 1;
            }
        }
        for ((mut node, sparse), vector) in selected.into_iter().zip(vectors) {
            let vector = if let Some((dense, sparse_name)) = names {
                json!({dense:vector,sparse_name:sparse})
            } else {
                vector
            };
            let Value::Object(mut payload) = node["payload"].take() else {
                return Err(INVALID);
            };
            payload.extend(common.clone());
            payload.insert("text".into(), node["text"].take());
            let document = node["doc_id"].as_str().filter(|id| !id.is_empty());
            payload.insert(
                "doc_id".into(),
                match document {
                    Some(id) => id.into(),
                    None => uuid()?.into(),
                },
            );
            let id = uuid()?;
            line.clear();
            serde_json::to_writer(
                &mut line,
                &json!({"id":id,"vector":vector,"payload":payload}),
            )
            .map_err(|_| INVALID)?;
            if line.len() >= 8 * 1024 * 1024 {
                return Err("INDEX_POINT_SIZE_LIMIT");
            }
            line.push(b'\n');
            writer.write_all(&line).map_err(|_| WRITE_FAILED)?;
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err("NO_SEARCHABLE_TEXT");
    }
    writer.flush().map_err(|_| WRITE_FAILED)?;
    Ok(
        json!({"ids":ids,"cache_hits":reused,"embedded":embedded,"embed_sec":embed_sec,
        "skipped_sparse":skipped}),
    )
}
