//! Bounded, deterministic, complete-or-refuse folder snapshots.
use super::*;
use std::collections::BTreeMap;

const MAX_FILES: usize = 25;
const MAX_BYTES: u64 = 50 * 1024 * 1024;
const MAX_CHARS: usize = 48000;
const INCOMPLETE: &str = "ATTACHMENT_FOLDER_INCOMPLETE";
const LIMIT: &str = "ATTACHMENT_FOLDER_LIMIT";

fn inventory(root: &Path) -> Result<BTreeMap<String, (PathBuf, u64)>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    let mut entries = 0;
    let mut bytes = 0;
    while let Some(folder) = stack.pop() {
        if !regular(&folder)? || folder.canonicalize().map_err(|_| INCOMPLETE)? != folder {
            return Err(INCOMPLETE);
        }
        for entry in fs::read_dir(&folder).map_err(|_| INCOMPLETE)? {
            let entry = entry.map_err(|_| INCOMPLETE)?;
            entries += 1;
            if entries > 5000 {
                return Err(LIMIT);
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_str().ok_or(INCOMPLETE)?;
            if matches!(
                name,
                ".git"
                    | ".venv"
                    | "__pycache__"
                    | ".pytest_cache"
                    | ".nicegui"
                    | ".claude"
                    | "CLAUDE"
                    | "QWEN"
                    | "_originals"
            ) || ["~$", ".~lock.", "._"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
            {
                continue;
            }
            match regular(&path) {
                Ok(true) => {}
                Err("ATTACHMENT_PATH_UNSAFE") => continue,
                _ => return Err(INCOMPLETE),
            }
            let meta = fs::symlink_metadata(&path).map_err(|_| INCOMPLETE)?;
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file()
                || !path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    matches!(
                        e.to_ascii_lowercase().as_str(),
                        "pdf"
                            | "docx"
                            | "doc"
                            | "pptx"
                            | "xls"
                            | "xlsx"
                            | "xlsm"
                            | "csv"
                            | "txt"
                            | "md"
                            | "json"
                            | "jsonl"
                            | "eml"
                            | "emlx"
                            | "msg"
                    )
                })
            {
                continue;
            }
            if !path
                .canonicalize()
                .map_err(|_| INCOMPLETE)?
                .starts_with(root)
            {
                return Err(INCOMPLETE);
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| INCOMPLETE)?
                .to_str()
                .ok_or(INCOMPLETE)?
                .replace('\\', "/");
            bytes += meta.len().min(MAX_BYTES + 1);
            if bytes > MAX_BYTES || files.len() >= MAX_FILES {
                return Err(LIMIT);
            }
            files.insert(relative, (path, meta.len()));
        }
    }
    Ok(files)
}
pub(super) fn snapshot(req: &Value, external: &impl Fn(&Value) -> Result<Value>) -> Result<Value> {
    let root = Path::new(req["path"].as_str().ok_or("ATTACHMENT_PATH_UNSAFE")?);
    if !root.is_absolute() {
        return Err("ATTACHMENT_PATH_UNSAFE");
    }
    let files = inventory(root)?;
    if files.is_empty() {
        return Err("ATTACHMENT_FOLDER_EMPTY");
    }
    let mut text = String::new();
    let mut chars = 0;
    let mut hashes = Vec::new();
    for (name, (path, size)) in &files {
        let (read, hash) = digest(&mut File::open(path).map_err(|_| INCOMPLETE)?)?;
        if read != *size {
            return Err(INCOMPLETE);
        }
        let converted = crate::document::conversion::execute_with(
            &json!({"schema":"les.document-convert.v1","path":path,
            "source_sha256":hash,"owned_state":req["owned_state"]}),
            external,
        )
        .map_err(|_| INCOMPLETE)?;
        let section = converted["text"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(INCOMPLETE)?;
        if converted["metadata"]["truncated"] == true {
            return Err(LIMIT);
        }
        let part = format!(
            "{}## {name}\n\n{section}",
            if text.is_empty() { "" } else { "\n\n" }
        );
        chars += part.chars().count();
        if chars > MAX_CHARS {
            return Err(LIMIT);
        }
        text.push_str(&part);
        hashes.push((path, hash));
    }
    if files != inventory(root)? {
        return Err(INCOMPLETE);
    }
    for (path, hash) in hashes {
        if digest(&mut File::open(path).map_err(|_| INCOMPLETE)?)?.1 != hash {
            return Err(INCOMPLETE);
        }
    }
    let name = root.file_name().and_then(|s| s.to_str()).unwrap_or("Папка");
    Ok(
        json!({"mode":"read","kind":"folder","name":name,"text":text,"chars":chars,
        "truncated":false,"file_count":files.len(),"files":files.keys().collect::<Vec<_>>(),"media_type":"document"}),
    )
}
