//! Explicit offline query-vector replay for the copy experiment, never a model fallback.
use super::*;

fn packet(cache: &Value, revision: &Value, question: &str, dimension: usize) -> Result<Vec<u8>> {
    if cache["schema"] != "les.edge-query-cache.v1"
        || cache["connection_revision_id"] != revision["revision_id"]
        || cache["model_id"] != revision["model_id"]
        || revision["locality"] != "loopback"
        || cache["dimension"].as_u64() != Some(dimension as u64)
        || cache["model_manifest_sha256"]
            .as_str()
            .is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("TRIAL_QUERY_CACHE_BINDING_INVALID".into());
    }
    let hash = les_core::sha256_hex(question.as_bytes());
    let row = cache["vectors"][&hash]
        .as_array()
        .filter(|v| v.len() == dimension)
        .ok_or("TRIAL_QUERY_CACHE_MISS")?;
    let vector = row
        .iter()
        .map(|v| {
            v.as_f64()
                .filter(|x| x.is_finite())
                .ok_or("TRIAL_QUERY_CACHE_INVALID")
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !norm.is_finite() || (norm - 1.).abs() > 0.0001 {
        return Err("TRIAL_QUERY_CACHE_INVALID".into());
    }
    let mut raw = format!("{}\n", json!({"count":1,"dimension":dimension})).into_bytes();
    for value in vector {
        raw.extend_from_slice(&value.to_le_bytes());
    }
    Ok(raw)
}

pub(super) async fn load(context: Arc<Context>, question: String) -> Result<Option<Vec<u8>>> {
    let Some(path) = std::env::var_os("LES_EXPERIMENT_QUERY_CACHE").map(PathBuf::from) else {
        return Ok(None);
    };
    #[cfg(not(feature = "edge"))]
    {
        let _ = (context, question, path);
        return Err("TRIAL_QUERY_CACHE_EDGE_REQUIRED".into());
    }
    #[cfg(feature="edge")]
    tokio::task::spawn_blocking(move || ->Result<_> {
        use std::{fs,io::Read};
        if !path.is_absolute() { return Err("TRIAL_QUERY_CACHE_INVALID".into()); }
        let edge = context.index_store.as_ref().and_then(|s|s.edge_root.as_ref()).ok_or("TRIAL_QUERY_CACHE_EDGE_REQUIRED")?;
        let marker:Value=serde_json::from_slice(&fs::read(edge.join("edge-bm25-trial.json")).map_err(|_|"EDGE_BM25_COPY_REQUIRED")?)
            .map_err(|_|"EDGE_BM25_COPY_INVALID")?;
        if marker["schema"]!="les.edge-bm25-trial.v1" || marker["phase"]!="ready"
            || fs::canonicalize(marker["root"].as_str().ok_or("EDGE_BM25_COPY_INVALID")?).map_err(|_|"EDGE_BM25_COPY_INVALID")?
                !=fs::canonicalize(edge).map_err(|_|"EDGE_BM25_COPY_INVALID")? {
            return Err("EDGE_BM25_COPY_INVALID".into());
        }
        let mut bytes=Vec::new(); fs::File::open(path).map_err(|_|"TRIAL_QUERY_CACHE_INVALID")?
            .take(4*1024*1024+1).read_to_end(&mut bytes).map_err(|_|"TRIAL_QUERY_CACHE_INVALID")?;
        if bytes.len()>4*1024*1024 { return Err("TRIAL_QUERY_CACHE_INVALID".into()); }
        let cache:Value=serde_json::from_slice(&bytes).map_err(|_|"TRIAL_QUERY_CACHE_INVALID")?;
        if fs::canonicalize(cache["state_root"].as_str().ok_or("TRIAL_QUERY_CACHE_INVALID")?).map_err(|_|"TRIAL_QUERY_CACHE_INVALID")?
            != fs::canonicalize(&context.state_root).map_err(|_|"TRIAL_QUERY_CACHE_INVALID")?
            || cache["source_points_sha256"]!=marker["source_points_sha256"] {
            return Err("TRIAL_QUERY_CACHE_BINDING_INVALID".into());
        }
        let execution=super::super::super::super::unwrap(crate::library::catalog(&context.database,"model_execution_snapshot",json!({"role":"embeddings"})))?;
        let revision=&execution["revision"];
        let settings=Settings::load(&context.state_root)?;
        let (_,contract_path)=identity(&context,&settings)?;
        let contract:Value=serde_json::from_slice(&fs::read(contract_path).map_err(|_|"INDEX_NOT_CONFIGURED")?).map_err(|_|"INDEX_SETTINGS_INVALID")?;
        if les_core::index_bootstrap::model_key(revision["model_id"].as_str().unwrap_or(""))
            !=les_core::index_bootstrap::model_key(contract["embedding_model"].as_str().unwrap_or("")) {
            return Err("EMBEDDING_INDEX_MODEL_MISMATCH".into());
        }
        let dimension=cache["dimension"].as_u64().filter(|d|*d>0 && *d<=65536).ok_or("TRIAL_QUERY_CACHE_INVALID")? as usize;
        let raw=packet(&cache,revision,&query_text(&settings,&question)?,dimension)?;
        eprintln!("{}",json!({"event":"edge_bm25_trial_cached_query","query_sha256":les_core::sha256_hex(question.as_bytes()),"dimension":dimension}));
        Ok(Some(raw))
    }).await.map_err(|_|"TRIAL_QUERY_CACHE_INVALID")?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> (Value, Value) {
        (
            json!({"schema":"les.edge-query-cache.v1","connection_revision_id":"copy:r1","model_id":"bge-m3",
            "dimension":3,"model_manifest_sha256":"a".repeat(64),"vectors":{les_core::sha256_hex(b"question"):[1.,0.,0.]}}),
            json!({"revision_id":"copy:r1","model_id":"bge-m3","locality":"loopback"}),
        )
    }
    #[test]
    fn a_pinned_query_vector_replays_but_an_unrecorded_query_never_falls_back() {
        let (cache, revision) = data();
        let raw = packet(&cache, &revision, "question", 3).unwrap();
        assert_eq!(
            raw.len() - raw.iter().position(|b| *b == b'\n').unwrap() - 1,
            24
        );
        assert!(packet(&cache, &revision, "unrecorded", 3)
            .unwrap_err()
            .code
            .contains("CACHE_MISS"));
    }
    #[test]
    fn cache_binding_dimension_and_normalization_are_required() {
        let (mut cache, mut revision) = data();
        assert!(packet(&cache, &revision, "question", 4).is_err());
        revision["revision_id"] = json!("copy:r2");
        assert!(packet(&cache, &revision, "question", 3).is_err());
        revision["revision_id"] = json!("copy:r1");
        cache["vectors"][les_core::sha256_hex(b"question")] = json!([0., 0., 0.]);
        assert!(packet(&cache, &revision, "question", 3).is_err());
    }
}
