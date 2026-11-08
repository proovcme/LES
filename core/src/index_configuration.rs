//! Reconstruct a frozen batch from the existing index contract, never from a model guess.
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

const MISMATCH: &str = "INDEX_CONTRACT_MISMATCH";
const INVALID: &str = "INDEX_SETTINGS_INVALID";
fn mismatch(field: &str) -> &'static str {
    eprintln!(
        "{}",
        json!({"event":"index_contract_rejected","field":field,"code":MISMATCH})
    );
    MISMATCH
}

fn setting<'a>(settings: &'a Value, key: &str, default: &'a str) -> &'a str {
    settings[key].as_str().unwrap_or(default)
}
fn number(
    settings: &Value,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, &'static str> {
    let value = match settings[key].as_str() {
        Some(value) => value.parse().map_err(|_| INVALID)?,
        None => default,
    };
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(INVALID)
    }
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, &'static str> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(MISMATCH)
}

pub fn execute(input: &Value) -> Result<Value, &'static str> {
    let contract = &input["contract"];
    let settings = &input["settings"];
    if contract["schema"] != "les.rag.index-contract.v3" {
        return Err("INDEX_NOT_CONFIGURED");
    }
    let mut unsigned = contract.clone();
    unsigned
        .as_object_mut()
        .ok_or(MISMATCH)?
        .remove("fingerprint");
    // Edge's dependencies enable serde_json/preserve_order. Identity must be
    // independent of Cargo feature unification and the input file's key order.
    unsigned.sort_all_objects();
    if contract["fingerprint"].as_str() != Some(&crate::sha256_hex(unsigned.to_string().as_bytes()))
    {
        return Err(mismatch("fingerprint"));
    }
    for (key, expected) in [
        ("document_embedding_mode", "raw-v1"),
        ("chunker_revision", "structure-aware-final-budget-v2"),
        ("qdrant_schema", "named"),
        ("hierarchy_schema", "les.rag.hierarchy.v1"),
        ("hierarchy_builder", "deterministic-heading-stack-v1"),
        ("hierarchy_retrieval", "soft-global-plus-descendant-rrf-v1"),
        ("navigation_evidence_policy", "navigation_not_evidence"),
    ] {
        if contract[key] != expected {
            return Err(mismatch(key));
        }
    }
    // Reject configuration drift before touching document status or loading a model.
    for (env, key) in [
        ("RAG_COLLECTION_NAME", "collection"),
        ("EMBEDDING_MODEL", "embedding_model"),
        ("EMBED_BACKEND", "embedding_backend"),
        ("RAG_DENSE_VECTOR_NAME", "dense_vector_name"),
        ("RAG_SPARSE_VECTOR_NAME", "sparse_vector_name"),
        ("RAG_SPARSE_TOKENIZER_REVISION", "sparse_tokenizer_revision"),
    ] {
        if let Some(value) = settings[env].as_str().filter(|v| !v.is_empty()) {
            if Some(value.trim()) != contract[key].as_str() {
                return Err(mismatch(key));
            }
        }
    }
    let profile = setting(settings, "LES_EMBED_PROFILE", "legacy")
        .trim()
        .to_lowercase();
    let (default_model, default_collection) = match profile.as_str() {
        "legacy" => ("BAAI/bge-m3", "les_rag"),
        "quality" => ("BAAI/bge-m3", "les_rag_bge_m3"),
        "qwen" => ("Qwen/Qwen3-Embedding-0.6B", "les_rag"),
        "fast" => ("intfloat/multilingual-e5-small", "les_rag_fast"),
        _ => return Err(INVALID),
    };
    let model = text(contract, "embedding_model")?;
    if contract
        .get("native_embedding_revision")
        .is_some_and(|v| v.as_str().is_none_or(|s| s.is_empty() || s.len() > 256))
    {
        return Err(MISMATCH);
    }
    if contract.get("native_embedding_revision").is_none()
        && !settings["EMBEDDING_MODEL"]
            .as_str()
            .is_some_and(|v| !v.is_empty())
    {
        let expected = if profile == "legacy" {
            setting(settings, "BGE_MODEL", default_model)
        } else {
            default_model
        };
        if model != expected {
            return Err(mismatch("embedding_model"));
        }
    }
    if setting(settings, "RAG_COLLECTION_NAME", default_collection) != text(contract, "collection")?
    {
        return Err(mismatch("collection"));
    }
    let dimension = contract["vector_size"]
        .as_u64()
        .filter(|v| (1..=65536).contains(v))
        .ok_or(MISMATCH)?;
    if number(settings, "RAG_VECTOR_SIZE", dimension, 1, 65536)? != dimension {
        return Err(mismatch("vector_size"));
    }
    let backend = text(contract, "embedding_backend")?;
    let mut descriptor = json!({"backend":backend,"model_id":model,"profile":profile,"vector_size":dimension.to_string()});
    if backend == "coreml" {
        for (env, key) in [
            ("COREML_EMBED_MODEL", "coreml_model"),
            ("COREML_EMBED_SEQ_LEN", "coreml_seq_len"),
            ("COREML_EMBED_COMPUTE_UNITS", "coreml_compute_units"),
            ("COREML_EMBED_FALLBACK", "coreml_fallback"),
        ] {
            descriptor[key] = json!(setting(settings, env, ""));
        }
    }
    let mut fields = descriptor
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap()))
        .collect::<Vec<_>>();
    fields.sort_unstable();
    let stable = fields.join("\n");
    let fingerprint = format!("{:x}", Sha1::digest(stable.as_bytes()));
    if contract["point_embedding_fingerprint"] != fingerprint {
        return Err(mismatch("point_embedding_fingerprint"));
    }
    let unit = text(contract, "chunk_unit")?;
    if !matches!(unit, "tokens" | "chars") {
        return Err(MISMATCH);
    }
    let size = contract["chunk_size"]
        .as_u64()
        .filter(|v| (1..=1_000_000).contains(v))
        .ok_or(MISMATCH)?;
    let overlap = contract["chunk_overlap"]
        .as_u64()
        .filter(|v| *v < size)
        .ok_or(MISMATCH)?;
    // A saved historical character contract remains in character mode unless explicitly changed.
    if settings["RAG_CHUNK_UNIT"]
        .as_str()
        .is_some_and(|v| v != unit)
    {
        return Err(MISMATCH);
    }
    let (size_key, overlap_key) = if unit == "tokens" {
        ("RAG_CHUNK_TOKENS", "RAG_CHUNK_OVERLAP_TOKENS")
    } else {
        ("RAG_CHUNK_SIZE", "RAG_CHUNK_OVERLAP")
    };
    let mut configured_size = number(settings, size_key, size, 1, 1_000_000)?;
    let configured_overlap = number(settings, overlap_key, overlap, 0, 1_000_000)?;
    if unit == "tokens" {
        let budget = number(settings, "COREML_EMBED_SEQ_LEN", 512, 33, 1_000_000)? - 32;
        if configured_size + configured_overlap > budget {
            configured_size = budget.saturating_sub(configured_overlap).max(64);
        }
    }
    if configured_size != size || configured_overlap != overlap {
        return Err(MISMATCH);
    }
    let tokenizer = if unit == "tokens" {
        let tokenizer = input["tokenizer"].clone();
        let info =
            crate::document::execute(&json!({"action":"tokenizer_info","tokenizer":tokenizer}))?;
        if contract
            .get("chunk_tokenizer_sha256")
            .is_some_and(|sha| sha != &info["sha256"])
        {
            return Err("DOCUMENT_TOKENIZER_CHANGED");
        }
        json!({"path":tokenizer["path"],"sha256":info["sha256"]})
    } else {
        Value::Null
    };
    let seconds: f64 = setting(settings, "RAG_PARSE_FILE_TIMEOUT_SEC", "1800")
        .parse()
        .map_err(|_| INVALID)?;
    if !seconds.is_finite() {
        return Err(INVALID);
    }
    let minimum = number(settings, "RAG_FINAL_MIN_CHUNK_CHARS", 20, 1, 1_000_000)?;
    let pdf_size = number(settings, "RAG_PDF_PAGE_NODE_MAX_CHARS", 1800, 1, 1_000_000)?;
    Ok(json!({
        "collection":contract["collection"],"embedding":descriptor,"embedding_fingerprint":fingerprint,
        "vector_names":{"dense":text(contract,"dense_vector_name")?,"sparse":text(contract,"sparse_vector_name")?},
        "embed_batch":number(settings,"RAG_EMBED_BATCH",16,1,512)?,"upsert_batch":number(settings,"RAG_UPSERT_BATCH",100,1,512)?,
        "verify_every":number(settings,"RAG_VERIFY_POINTS_EVERY",1,1,1_000_000)?,
        "cache_enabled":(["1","true","yes","on"].contains(&setting(settings,"RAG_CHUNK_HASH_CACHE","true").to_lowercase().as_str())),
        "max_attempts":number(settings,"RAG_PARSE_MAX_ATTEMPTS",4,1,100)?,"release_workers_after":true,
        "chunking":{"unit":unit,"chunk_size":size,"chunk_overlap":overlap},
        "node_preparation":{"unit":unit,"size":size,"minimum":minimum,"window":number(settings,"RAG_PARENT_WINDOW_CHUNKS",4,1,1000)?,"tokenizer":tokenizer},
        "format_worker":{"timeout_ms":(seconds.clamp(0.1,3600.)*1000.) as u64,
            "minimum":number(settings,"RAG_MIN_CHUNK_CHARS",100,1,1_000_000)?,"final_minimum":minimum,
            "table_row_limit":number(settings,"RAG_TABLE_ROW_INDEX_MAX_CHUNKS",600,1,1_000_000)?,
            "pdf_page_size":pdf_size,"pdf_page_overlap":number(settings,"RAG_PDF_PAGE_NODE_OVERLAP_CHARS",150,0,1_000_000)?.min(pdf_size/3)}
    }))
}
