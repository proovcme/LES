//! Native intake uses the persisted index contract and explicitly assigned model role.
use crate::{library::catalog, model_runtime, Context};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

fn read(path: &Path) -> Result<Value, &'static str> {
    let file = File::open(path).map_err(|_| "INDEX_NOT_CONFIGURED")?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "INDEX_SETTINGS_INVALID")?;
    if bytes.len() > 1024 * 1024 {
        return Err("INDEX_SETTINGS_INVALID");
    }
    serde_json::from_slice(&bytes).map_err(|_| "INDEX_SETTINGS_INVALID")
}

pub(crate) struct Settings {
    values: BTreeMap<String, String>,
}
impl Settings {
    pub(crate) fn load(state: &Path) -> Result<Self, &'static str> {
        let file = std::env::var_os("LES_ENV_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| state.join(".env"));
        let mut values = les_core::model_secrets::settings_values(&file)
            .map_err(|_| "INDEX_SETTINGS_INVALID")?;
        values.extend(std::env::vars());
        for value in values.values_mut() {
            let v = value.trim();
            *value = if v.len() >= 2
                && ((v.starts_with('\'') && v.ends_with('\''))
                    || (v.starts_with('"') && v.ends_with('"')))
            {
                v[1..v.len() - 1].to_owned()
            } else {
                v.to_owned()
            };
        }
        Ok(Self { values })
    }
    pub(crate) fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.values.get(key).map(String::as_str).unwrap_or(default)
    }
    pub(crate) fn core_values(&self) -> Value {
        // Credential values are never passed into a batch or its durable result.
        Value::Object(
            self.values
                .iter()
                .filter(|(key, _)| {
                    [
                        "LES_EMBED_PROFILE",
                        "EMBEDDING_MODEL",
                        "EMBED_BACKEND",
                        "BGE_MODEL",
                        "RAG_COLLECTION_NAME",
                        "RAG_VECTOR_SIZE",
                        "RAG_DENSE_VECTOR_NAME",
                        "RAG_SPARSE_VECTOR_NAME",
                        "RAG_SPARSE_TOKENIZER_REVISION",
                        "RAG_CHUNK_UNIT",
                        "RAG_CHUNK_TOKENS",
                        "RAG_CHUNK_OVERLAP_TOKENS",
                        "RAG_CHUNK_SIZE",
                        "RAG_CHUNK_OVERLAP",
                        "RAG_FINAL_MIN_CHUNK_CHARS",
                        "RAG_MIN_CHUNK_CHARS",
                        "RAG_PARSE_FILE_TIMEOUT_SEC",
                        "RAG_EMBED_BATCH",
                        "RAG_UPSERT_BATCH",
                        "RAG_VERIFY_POINTS_EVERY",
                        "RAG_CHUNK_HASH_CACHE",
                        "RAG_PARSE_MAX_ATTEMPTS",
                        "RAG_PARENT_WINDOW_CHUNKS",
                        "RAG_TABLE_ROW_INDEX_MAX_CHUNKS",
                        "RAG_PDF_PAGE_NODE_MAX_CHARS",
                        "RAG_PDF_PAGE_NODE_OVERLAP_CHARS",
                        "COREML_EMBED_MODEL",
                        "COREML_EMBED_SEQ_LEN",
                        "COREML_EMBED_COMPUTE_UNITS",
                        "COREML_EMBED_FALLBACK",
                    ]
                    .contains(&key.as_str())
                })
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect::<Map<_, _>>(),
        )
    }
}

fn expand(raw: &str) -> Result<PathBuf, &'static str> {
    let path = Path::new(raw);
    let path = if let Ok(rest) = path.strip_prefix("~") {
        PathBuf::from(
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .ok_or("INDEX_SETTINGS_INVALID")?,
        )
        .join(rest)
    } else {
        path.to_owned()
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("INDEX_SETTINGS_INVALID")
    }
}

pub(crate) fn tokenizer(settings: &Settings, contract: &Value) -> Result<Value, &'static str> {
    if contract["chunk_unit"] != "tokens" {
        return Ok(Value::Null);
    }
    let explicit = settings.get("RAG_TOKENIZER_FILE", "");
    if !explicit.is_empty() {
        return Ok(json!({"path":expand(explicit)?}));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("DOCUMENT_TOKENIZER_UNAVAILABLE")?;
    let hf_home = expand(
        settings.get(
            "HF_HOME",
            home.join(".cache/huggingface")
                .to_str()
                .ok_or("DOCUMENT_TOKENIZER_UNAVAILABLE")?,
        ),
    )?;
    let hub = expand(
        settings.get(
            "HF_HUB_CACHE",
            settings.get(
                "HUGGINGFACE_HUB_CACHE",
                hf_home
                    .join("hub")
                    .to_str()
                    .ok_or("DOCUMENT_TOKENIZER_UNAVAILABLE")?,
            ),
        ),
    )?;
    let model = contract["embedding_model"]
        .as_str()
        .ok_or("INDEX_CONTRACT_MISMATCH")?;
    if model.contains("..")
        || model.split('/').count() > 2
        || !model
            .chars()
            .all(|c| c.is_alphanumeric() || "_./-".contains(c))
    {
        return Err("DOCUMENT_TOKENIZER_UNAVAILABLE");
    }
    let repository = hub.join(format!("models--{}", model.replace('/', "--")));
    let revision = std::fs::read_to_string(repository.join("refs/main")).unwrap_or_default();
    let revision = revision.trim();
    if (40..=64).contains(&revision.len())
        && revision
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        let path = repository
            .join("snapshots")
            .join(revision)
            .join("tokenizer.json");
        return Ok(json!({"path":path}));
    }
    let candidates = std::fs::read_dir(repository.join("snapshots"))
        .map_err(|_| "DOCUMENT_TOKENIZER_UNAVAILABLE")?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("tokenizer.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err("DOCUMENT_TOKENIZER_UNAVAILABLE");
    }
    Ok(json!({"path":candidates[0]}))
}

pub(crate) fn identity(
    context: &Context,
    settings: &Settings,
) -> Result<(String, PathBuf), &'static str> {
    let collection = settings.get(
        "RAG_COLLECTION_NAME",
        match settings.get("LES_EMBED_PROFILE", "legacy") {
            "quality" => "les_rag_bge_m3",
            "fast" => "les_rag_fast",
            _ => "les_rag",
        },
    );
    let mut cleaned = String::new();
    let mut invalid = false;
    for c in collection.chars() {
        if c.is_ascii_alphanumeric() || "_.-".contains(c) {
            cleaned.push(c);
            invalid = false;
        } else if !invalid {
            cleaned.push('_');
            invalid = true;
        }
    }
    let default = context.database.with_file_name(format!(
        "{}.{}.index-contract.json",
        context
            .database
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("INDEX_SETTINGS_INVALID")?,
        cleaned.trim_matches(['.', '_'])
    ));
    let path = if settings.get("RAG_INDEX_CONTRACT_PATH", "").is_empty() {
        default
    } else {
        expand(settings.get("RAG_INDEX_CONTRACT_PATH", ""))?
    };
    Ok((collection.into(), path))
}

pub(crate) fn intake(
    context: &Context,
    job: &Value,
    sources: Value,
) -> Result<Value, &'static str> {
    let settings = Settings::load(&context.state_root)?;
    let (_, path) = identity(context, &settings)?;
    let contract = read(&path)?;
    let mut input = les_core::index_configuration::execute(
        &json!({"contract":contract,"settings":settings.core_values(),"tokenizer":tokenizer(&settings,&contract)?}),
    )?;
    let binding = catalog(
        &context.database,
        "model_execution_snapshot",
        json!({"role":"embeddings"}),
    );
    if binding["status"] != "completed" {
        return Err("MODEL_REGISTRY_UNAVAILABLE");
    }
    // No role -> no hidden legacy/local fallback. A saved local ONNX revision is also a role.
    if binding["value"].is_null() {
        return Err("EMBEDDING_MODEL_NOT_ASSIGNED");
    }
    let attachment = job["root"] == les_core::catalog::INDEXED_ATTACHMENT_ROOT;
    if attachment && binding["value"]["revision"]["locality"] == "remote" {
        return Err("ATTACHMENT_INDEX_LOCAL_MODEL_REQUIRED");
    }
    let env = std::env::var_os("LES_ENV_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| context.state_root.join(".env"));
    let resolved = model_runtime::resolve(&binding["value"], "embeddings", &env)
        .map_err(|cause| {
            eprintln!("{}",json!({"event":"intake_model_unavailable","code":cause.split(':').next().unwrap_or("MODEL_RUNTIME_UNAVAILABLE")}));
            "EMBEDDING_MODEL_NOT_READY"
        })?;
    if super::model_key(resolved.revision["model_id"].as_str().unwrap_or_default())
        != super::model_key(input["embedding"]["model_id"].as_str().unwrap_or_default())
    {
        return Err("EMBEDDING_INDEX_MODEL_MISMATCH");
    }
    input["embedding_execution"] = json!({"revision_id":resolved.revision["revision_id"],"allow_remote":resolved.endpoint.locality=="remote"});
    input["schema"] = json!("les.index-batch.v1");
    input["database"] = json!(context.database);
    input["source_root"] = json!(context.state_root.join("storage/datasets"));
    input["content_root"] = json!(context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?
        .content_root(context));
    input["spool_directory"] = json!(context.state_root.join("data/index-publish"));
    input["dataset_id"] = job["dataset_id"].clone();
    input["intake_id"] = job["id"].clone();
    input["limit"] = json!(sources.as_array().ok_or("INDEX_BATCH_INVALID")?.len());
    input["sources"] = sources;
    if attachment {
        input["expected_source_sha256"] = job["attachment_sha256"].clone();
    }
    Ok(input)
}
