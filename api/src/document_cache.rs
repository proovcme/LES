//! A complete Word conversion, shared by indexing and physical reads.
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, &'static str>;
pub(crate) fn processor_revision() -> Result<String> {
    static REV: std::sync::OnceLock<Result<String>> = std::sync::OnceLock::new();
    REV.get_or_init(|| {
        let path = std::env::current_exe().map_err(|_| "SOURCE_READER_UNAVAILABLE")?;
        les_core::pdf_source::fingerprint(
            &mut File::open(path).map_err(|_| "SOURCE_READER_UNAVAILABLE")?,
        )
    })
    .clone()
}
fn binding(input: &Value) -> Result<Option<(PathBuf, String)>> {
    let Some(state) = input["owned_state"].as_str() else {
        return Ok(None);
    };
    let state = Path::new(state)
        .canonicalize()
        .map_err(|_| "DOCUMENT_PATH_INVALID")?;
    let source = input["source_sha256"]
        .as_str()
        .ok_or("DOCUMENT_PATH_INVALID")?;
    if source.len() != 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("DOCUMENT_PATH_INVALID");
    }
    let path = Path::new(input["path"].as_str().ok_or("DOCUMENT_PATH_INVALID")?);
    if !path.is_absolute() {
        return Err("DOCUMENT_PATH_INVALID");
    }
    let file = File::open(path).map_err(|_| "DOCUMENT_READ_FAILED")?;
    if !file
        .metadata()
        .map_err(|_| "DOCUMENT_READ_FAILED")?
        .is_file()
    {
        return Err("DOCUMENT_READ_FAILED");
    }
    if file.metadata().map_err(|_| "DOCUMENT_READ_FAILED")?.len() > 64 * 1024 * 1024 {
        return Err("DOCUMENT_FILE_LIMIT");
    }
    let mut file = file;
    if les_core::pdf_source::fingerprint(&mut file)? != source {
        return Err("SOURCE_REVISION_CHANGED");
    }
    let helper = input["ocr"]["helper"]
        .as_str()
        .and_then(|path| File::open(path).ok())
        .and_then(|mut file| les_core::pdf_source::fingerprint(&mut file).ok());
    let key=les_core::sha256_hex(les_core::tool_execution::canonical_json(&json!({"schema":"les.word-conversion.v3","build":include_str!("../../config/version.json"),
        "sha256":source,"format":path.extension().and_then(|x|x.to_str()).map(str::to_ascii_lowercase),
        "parser":crate::document_parser::revision()?,"processor":processor_revision()?,"word_parser":input["word_parser"],
        "ocr":input["ocr"],"ocr_unavailable":input["ocr_unavailable"],"helper":helper}))?.as_bytes());
    Ok(Some((state, key)))
}
fn manifest(state: &Path, source: &str) -> Option<String> {
    let root = state.join("storage/docx-parts").join(source);
    if root.canonicalize().ok()? != root {
        return None;
    }
    let pointer = root.join("manifest.json");
    let meta = fs::symlink_metadata(&pointer).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 {
        return None;
    }
    let pointer: Value = serde_json::from_slice(&fs::read(pointer).ok()?).ok()?;
    let sha = pointer["sha256"].as_str()?;
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let path = root.join(sha);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 8 * 1024 * 1024 {
        return None;
    }
    (les_core::sha256_hex(&fs::read(path).ok()?) == sha).then(|| sha.to_owned())
}
pub(crate) fn identity(input: &Value) -> Result<Option<String>> {
    Ok(binding(input)?.map(|(_, key)| key))
}
pub(crate) fn load(input: &Value) -> Result<Option<Value>> {
    let Some((state, key)) = binding(input)? else {
        return Ok(None);
    };
    let Some(entry) = crate::derived_cache::load(&state, "word", &key) else {
        eprintln!(
            "{}",
            json!({"event":"word_conversion_cache_miss","key":key,"reason":"entry_missing_or_invalid"})
        );
        return Ok(None);
    };
    let value = &entry["value"];
    if value["source_sha256"] != input["source_sha256"]
        || value["status"] != "completed"
        || value["text"].as_str().is_none_or(|s| s.trim().is_empty())
    {
        eprintln!(
            "{}",
            json!({"event":"word_conversion_cache_miss","key":key,"reason":"invalid_conversion"})
        );
        return Ok(None);
    }
    if value["metadata"]["docx_parts_count"]
        .as_u64()
        .is_some_and(|n| n > 0)
        && entry["manifest"].as_str().is_none_or(|expected| {
            manifest(&state, input["source_sha256"].as_str().unwrap()).as_deref() != Some(expected)
        })
    {
        eprintln!(
            "{}",
            json!({"event":"word_conversion_cache_miss","key":key,"reason":"manifest_changed_or_missing"})
        );
        return Ok(None);
    }
    eprintln!(
        "{}",
        json!({"event":"word_conversion_cache_hit","source_sha256":input["source_sha256"]})
    );
    Ok(Some(value.clone()))
}
pub(crate) fn store(input: &Value, value: &Value) -> Result<()> {
    let Some((state, key)) = binding(input)? else {
        return Ok(());
    };
    if value["source_sha256"] != input["source_sha256"]
        || value["status"] != "completed"
        || value["text"].as_str().is_none_or(|s| s.trim().is_empty())
    {
        return Ok(());
    }
    let manifest = manifest(&state, input["source_sha256"].as_str().unwrap());
    if value["metadata"]["docx_parts_count"]
        .as_u64()
        .is_some_and(|n| n > 0)
        && manifest.is_none()
    {
        return Ok(());
    }
    crate::derived_cache::store(
        &state,
        "word",
        &key,
        &json!({"value":value,"manifest":manifest}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_revision_settings_and_parts_manifest_bind_a_persisted_conversion() {
        let root = std::env::temp_dir().join(format!("les-word-cache-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("original.docx");
        fs::write(&path, b"revision one").unwrap();
        let sha = les_core::sha256_hex(b"revision one");
        let input = json!({"owned_state":root,"path":path,"source_sha256":sha,"word_parser":"native","ocr":{"language":"ru"}});
        assert!(load(&input).unwrap().is_none());
        let value =
            json!({"source_sha256":sha,"status":"completed","text":"Exact 17.25","metadata":{}});
        store(&input, &value).unwrap();
        assert_eq!(load(&input).unwrap(), Some(value.clone()));
        let mut reordered = input.clone();
        reordered["ocr"] = serde_json::from_str("{\"second\":2,\"first\":1}").unwrap();
        let a = identity(&reordered).unwrap();
        reordered["ocr"] = serde_json::from_str("{\"first\":1,\"second\":2}").unwrap();
        assert_eq!(identity(&reordered).unwrap(), a);
        let mut changed = input.clone();
        changed["ocr"]["language"] = json!("en");
        assert!(load(&changed).unwrap().is_none());
        let mut invalid = value.clone();
        invalid["text"] = json!("");
        store(&changed, &invalid).unwrap();
        assert!(load(&changed).unwrap().is_none());
        invalid = value.clone();
        invalid["metadata"]["docx_parts_count"] = json!(1);
        store(&changed, &invalid).unwrap();
        assert!(load(&changed).unwrap().is_none());
        fs::write(&path, b"revision two").unwrap();
        assert_eq!(load(&input).unwrap_err(), "SOURCE_REVISION_CHANGED");
        changed["source_sha256"] = json!(les_core::sha256_hex(b"revision two"));
        assert!(load(&changed).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
