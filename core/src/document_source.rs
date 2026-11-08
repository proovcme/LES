//! Resolve original files from server-owned catalog records and storage roots.
//! Display names or paths supplied by the model never establish authority.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
}

pub fn resolve(
    recorded: &str,
    dataset: &str,
    name: &str,
    storage: &Path,
) -> Result<PathBuf, &'static str> {
    let recorded = crate::auth::trim(recorded);
    if !recorded.is_empty() {
        let path = Path::new(recorded);
        if !path.is_absolute() {
            return Err("SOURCE_PATH_INVALID");
        }
        // A recorded external original must not silently resolve to a different
        // internal file when the external file disappears.
        let path = path.canonicalize().map_err(|_| "SOURCE_UNAVAILABLE")?;
        return if path.is_file() {
            Ok(path)
        } else {
            Err("SOURCE_UNAVAILABLE")
        };
    }
    if !storage.is_absolute() || !component(dataset) || name.is_empty() {
        return Err("SOURCE_PATH_INVALID");
    }
    let parts: Vec<_> = name.split(['/', '\\']).collect();
    if parts.iter().any(|part| !component(part)) {
        return Err("SOURCE_PATH_INVALID");
    }
    let storage = storage.canonicalize().map_err(|_| "SOURCE_UNAVAILABLE")?;
    let root = storage
        .join(dataset)
        .canonicalize()
        .map_err(|_| "SOURCE_UNAVAILABLE")?;
    // Reject both a substituted dataset root and a nested junction/symlink.
    if !root.starts_with(&storage) {
        return Err("SOURCE_PATH_INVALID");
    }
    let path = parts
        .iter()
        .fold(root.clone(), |path, part| path.join(part));
    let path = path.canonicalize().map_err(|_| "SOURCE_UNAVAILABLE")?;
    if !path.starts_with(&root) {
        return Err("SOURCE_PATH_INVALID");
    }
    if !path.is_file() {
        return Err("SOURCE_UNAVAILABLE");
    }
    Ok(path)
}

fn public_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        if let Some(disk) = text.strip_prefix(r"\\?\") {
            if disk.as_bytes().get(1) == Some(&b':') {
                return PathBuf::from(disk);
            }
        }
    }
    path
}

pub fn execute(request: &Value) -> Value {
    let document = &request["document"];
    let text = |key| document[key].as_str().unwrap_or("");
    match resolve(
        text("source_path"),
        text("dataset_id"),
        text("file_name"),
        Path::new(request["content_root"].as_str().unwrap_or("")),
    ) {
        Ok(path) => json!({"status":"ok","path":public_path(path)}),
        Err(code) => json!({"status":"error","code":code}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_originals_cannot_escape_or_replace_missing_external_sources() {
        let root = std::env::temp_dir().join(format!(
            "les-original-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage = root.join("datasets");
        let folder = storage.join("dataset/folder");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("Документ.pdf");
        std::fs::write(&file, b"original").unwrap();
        assert_eq!(
            resolve("", "dataset", "folder/Документ.pdf", &storage).unwrap(),
            file.canonicalize().unwrap()
        );
        assert_eq!(
            resolve("", "dataset", "folder\\Документ.pdf", &storage).unwrap(),
            file.canonicalize().unwrap()
        );
        for (dataset, name) in [
            ("..", "folder/Документ.pdf"),
            ("dataset", "../Документ.pdf"),
            ("dataset", "C:Doc.pdf"),
            ("dataset", "/folder/Документ.pdf"),
            ("dataset", "folder//Doc.pdf"),
        ] {
            assert_eq!(
                resolve("", dataset, name, &storage).err(),
                Some("SOURCE_PATH_INVALID")
            );
        }
        assert_eq!(
            resolve(
                root.join("missing.pdf").to_str().unwrap(),
                "dataset",
                "folder/Документ.pdf",
                &storage
            )
            .err(),
            Some("SOURCE_UNAVAILABLE")
        );
        assert_eq!(
            resolve("relative.pdf", "dataset", "folder/Документ.pdf", &storage).err(),
            Some("SOURCE_PATH_INVALID")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
