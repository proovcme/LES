//! Complete read-only enumeration. An unreadable subtree never means an empty folder.
use serde_json::{json, Value};
use std::{collections::HashSet, path::PathBuf};

pub fn scan(request: &Value) -> Result<Value, &'static str> {
    let root = PathBuf::from(request["path"].as_str().ok_or("CORE_SCAN_PATH_INVALID")?);
    if !root.is_absolute() || !root.is_dir() {
        return Err("CORE_SCAN_PATH_INVALID");
    }
    let limit = request["limit"]
        .as_u64()
        .filter(|n| (1..=50000).contains(n))
        .ok_or("CORE_SCAN_LIMIT_INVALID")? as usize;
    let skip = request["skip_directories"]
        .as_array()
        .ok_or("CORE_SCAN_SKIP_INVALID")?;
    if skip.len() > 128 {
        return Err("CORE_SCAN_SKIP_INVALID");
    }
    let skip: HashSet<&str> = skip
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| s.len() <= 255)
                .ok_or("CORE_SCAN_SKIP_INVALID")
        })
        .collect::<Result<_, _>>()?;
    let mut directories = vec![root];
    let mut files = Vec::new();
    let mut bytes = 0usize;
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(directory).map_err(|_| "CORE_SCAN_UNAVAILABLE")?;
        for entry in entries {
            let entry = entry.map_err(|_| "CORE_SCAN_UNAVAILABLE")?;
            let kind = entry.file_type().map_err(|_| "CORE_SCAN_UNAVAILABLE")?;
            let path = entry.path();
            if kind.is_symlink() {
                continue;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if entry
                    .metadata()
                    .map_err(|_| "CORE_SCAN_UNAVAILABLE")?
                    .file_attributes()
                    & 0x400
                    != 0
                {
                    continue;
                }
            }
            if kind.is_dir() {
                if !skip.contains(entry.file_name().to_str().ok_or("CORE_SCAN_NAME_INVALID")?) {
                    directories.push(path);
                }
            } else if kind.is_file() {
                if files.len() >= limit {
                    return Err("CORE_SCAN_FILE_LIMIT");
                }
                let text = path.to_str().ok_or("CORE_SCAN_NAME_INVALID")?.to_owned();
                bytes = bytes
                    .checked_add(text.len())
                    .ok_or("CORE_SCAN_OUTPUT_LIMIT")?;
                if bytes > 8 * 1024 * 1024 {
                    return Err("CORE_SCAN_OUTPUT_LIMIT");
                }
                files.push(text);
            }
        }
    }
    files.sort();
    Ok(json!({"files":files,"complete":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_skips_and_limits_preserve_complete_inventory() {
        let root = std::env::temp_dir().join(format!("les-scan-{}", std::process::id()));
        std::fs::create_dir_all(root.join("_originals")).unwrap();
        std::fs::write(root.join("Книга 日本語.txt"), b"fixture").unwrap();
        std::fs::write(root.join("_originals/skip.txt"), b"skip").unwrap();
        let request = json!({"path":root,"limit":1,"skip_directories":["_originals"]});
        let result = scan(&request).unwrap();
        assert_eq!(result["files"].as_array().unwrap().len(), 1);
        assert_eq!(result["complete"], true);
        assert_eq!(
            scan(&json!({"path":root,"limit":1,"skip_directories":[]})).unwrap_err(),
            "CORE_SCAN_FILE_LIMIT"
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(scan(&request).unwrap_err(), "CORE_SCAN_PATH_INVALID");
    }
}
