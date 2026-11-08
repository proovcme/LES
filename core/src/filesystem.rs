//! Bounded, read-only file tools. Root paths are server policy, never model arguments.
use crate::workspace::{Failure, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};
const FORBIDDEN: &[&str] = &[
    ".env",
    ".git",
    ".venv",
    "__pycache__",
    "data",
    "dist",
    "logs",
    "local_private_archive",
];
const TEXT: &[&str] = &[
    "txt", "md", "json", "jsonl", "csv", "tsv", "xml", "yaml", "yml", "html", "svg", "py", "ini",
    "cfg", "sql", "log",
];
fn fail(code: &'static str, detail: &str) -> Failure {
    Failure::new(code, detail)
}
fn denied() -> Failure {
    fail(
        "FILESYSTEM_SCOPE_DENIED",
        "Путь находится вне разрешённой папки или содержит ссылку/закрытый каталог.",
    )
}
fn unavailable() -> Failure {
    fail(
        "FILESYSTEM_UNAVAILABLE",
        "Не удалось прочитать файл или папку. Проверьте доступ и подключение диска.",
    )
}
fn forbidden(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_str()
            .is_none_or(|s| FORBIDDEN.contains(&s.to_lowercase().as_str()))
    })
}
fn hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_none_or(|s| s.starts_with('.') || FORBIDDEN.contains(&s.to_lowercase().as_str()))
}
fn link(path: &Path) -> Result<bool> {
    let value = fs::symlink_metadata(path).map_err(|_| unavailable())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if value.file_attributes() & 0x400 != 0 {
            return Ok(true);
        }
    }
    Ok(value.file_type().is_symlink())
}
fn roots(request: &Value) -> Result<BTreeMap<String, PathBuf>> {
    let values = request["roots"]
        .as_object()
        .filter(|v| v.len() <= 128)
        .ok_or_else(denied)?;
    values
        .iter()
        .map(|(key, value)| {
            let path = PathBuf::from(value.as_str().ok_or_else(denied)?);
            if key.is_empty() || key.len() > 80 || !path.is_absolute() {
                return Err(denied());
            }
            Ok((key.clone(), path))
        })
        .collect()
}
struct Scope {
    key: String,
    root: PathBuf,
}
impl Scope {
    fn target(&self, relative: &str) -> Result<PathBuf> {
        if relative.len() > 16384 {
            return Err(denied());
        }
        let path = Path::new(relative);
        if path.is_absolute() {
            return Err(denied());
        }
        let mut target = self.root.clone();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::Normal(name) => {
                    let name = name.to_str().ok_or_else(denied)?;
                    if name.starts_with('.')
                        || name.contains(':')
                        || FORBIDDEN.contains(&name.to_lowercase().as_str())
                    {
                        return Err(denied());
                    }
                    target.push(name);
                    if target.try_exists().map_err(|_| unavailable())? && link(&target)? {
                        return Err(denied());
                    }
                }
                _ => return Err(denied()),
            }
        }
        self.check(&target)?;
        Ok(target)
    }
    fn check(&self, path: &Path) -> Result<()> {
        if forbidden(path) || !path.starts_with(&self.root) {
            return Err(denied());
        }
        if path.try_exists().map_err(|_| unavailable())? {
            if link(path)? {
                return Err(denied());
            }
            let canonical = path.canonicalize().map_err(|_| unavailable())?;
            if !canonical.starts_with(&self.root) || forbidden(&canonical) {
                return Err(denied());
            }
        }
        Ok(())
    }
    fn metadata(&self, path: &Path) -> Result<Value> {
        self.check(path)?;
        let meta = path.metadata().map_err(|_| unavailable())?;
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| denied())?
            .to_str()
            .ok_or_else(denied)?;
        let suffix = path
            .extension()
            .and_then(|v| v.to_str())
            .filter(|s| !s.is_empty())
            .map(|v| format!(".{v}"))
            .unwrap_or_default();
        Ok(
            json!({"root":self.key,"path":relative,"name":path.file_name().and_then(|v|v.to_str()).unwrap_or(&self.key),
            "is_dir":meta.is_dir(),"is_file":meta.is_file(),"suffix":suffix,"size":meta.len(),
            "mtime":meta.modified().ok().and_then(|v|v.duration_since(UNIX_EPOCH).ok()).map(|v|v.as_secs_f64()).unwrap_or(0.)}),
        )
    }
    fn source(&self, path: &Path) -> Result<Value> {
        let value = self.metadata(path)?;
        Ok(json!({"kind":"filesystem","root":self.key,"path":value["path"],"name":value["name"]}))
    }
    fn open(&self, path: &Path) -> Result<File> {
        self.check(path)?;
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;
            File::options().read(true).share_mode(1).open(path)
        };
        #[cfg(not(windows))]
        let file = File::open(path);
        let file = file.map_err(|_| unavailable())?;
        if !file.metadata().map_err(|_| unavailable())?.is_file() {
            return Err(unavailable());
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            #[link(name = "kernel32")]
            extern "system" {
                fn GetFinalPathNameByHandleW(
                    file: *mut std::ffi::c_void,
                    name: *mut u16,
                    length: u32,
                    flags: u32,
                ) -> u32;
            }
            let mut name = vec![0u16; 32768];
            // SAFETY: a live File owns the handle and the buffer has the supplied capacity.
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle(),
                    name.as_mut_ptr(),
                    name.len() as u32,
                    0,
                )
            };
            if length == 0 || length as usize >= name.len() {
                return Err(unavailable());
            }
            let name = String::from_utf16(&name[..length as usize]).map_err(|_| denied())?;
            if !Path::new(&name).starts_with(&self.root) || forbidden(Path::new(&name)) {
                return Err(denied());
            }
        }
        self.check(path)?;
        Ok(file)
    }
    fn text(&self, path: &Path, limit: u64) -> Result<(String, bool, String)> {
        let file = self.open(path)?;
        let before = file.metadata().map_err(|_| unavailable())?;
        if before.len() > limit {
            return Err(fail("FILESYSTEM_TEXT_LIMIT","Файл слишком большой для чтения целиком. Используйте чтение документа по разделам."));
        }
        let mut bytes = Vec::new();
        (&file)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        if bytes.len() as u64 > limit {
            return Err(fail(
                "FILESYSTEM_TEXT_LIMIT",
                "Файл изменился или превышает предел чтения.",
            ));
        }
        let text = String::from_utf8_lossy(&bytes);
        let replaced = matches!(text, std::borrow::Cow::Owned(_));
        let after = file.metadata().map_err(|_| unavailable())?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(fail(
                "FILESYSTEM_CHANGED",
                "Файл изменился во время чтения. Повторите запрос.",
            ));
        }
        Ok((text.into_owned(), replaced, crate::sha256_hex(&bytes)))
    }
    fn children(&self, path: &Path) -> Result<(Vec<PathBuf>, usize)> {
        self.check(path)?;
        let mut values = Vec::new();
        let mut excluded = 0;
        for (index, entry) in fs::read_dir(path).map_err(|_| unavailable())?.enumerate() {
            if index >= 20000 {
                return Err(fail(
                    "FILESYSTEM_LIST_LIMIT",
                    "В папке слишком много записей. Выберите вложенную папку.",
                ));
            }
            let value = entry.map_err(|_| unavailable())?.path();
            if !hidden(&value) && !link(&value)? {
                values.push(value);
            } else {
                excluded += 1;
            }
        }
        values.sort_by_cached_key(|v| {
            crate::unicode::casefold(v.file_name().and_then(|v| v.to_str()).unwrap_or(""))
        });
        self.check(path)?;
        Ok((values, excluded))
    }
    fn tree(&self, path: &Path, depth: u64, remaining: &mut usize) -> Result<Value> {
        if *remaining == 0 {
            return Err(fail(
                "FILESYSTEM_LIST_LIMIT",
                "Дерево папок слишком большое. Уменьшите глубину или выберите вложенную папку.",
            ));
        }
        *remaining -= 1;
        let mut value = self.metadata(path)?;
        if value["is_dir"] == true && depth > 0 {
            let (mut entries, excluded) = self.children(path)?;
            if excluded > 0 {
                value["excluded_entries"] = json!(excluded);
            }
            entries.sort_by_key(|v| !v.is_dir());
            let omitted = entries.len().saturating_sub(500);
            let mut children = Vec::new();
            for path in entries.iter().take(500) {
                children.push(self.tree(path, depth - 1, remaining)?);
            }
            value["children"] = json!(children);
            if omitted > 0 {
                value["children_truncated"] = json!(true);
            }
        }
        Ok(value)
    }
}
fn number(value: &Value, default: u64, min: u64, max: u64) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .unwrap_or(default)
        .clamp(min, max)
}
fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| TEXT.contains(&s.to_lowercase().as_str()))
}
fn excerpt(text: &str, needle: &str, length: usize) -> Option<String> {
    let folded = crate::unicode::casefold(text);
    let pos = folded.find(needle)?;
    let char_pos = folded[..pos].chars().count();
    let mut covered = 0;
    let mut start = None;
    for (index, c) in text.chars().enumerate() {
        let value = crate::unicode::casefold(&c.to_string());
        covered += value.chars().count();
        if covered > char_pos {
            start = Some(index);
            break;
        }
    }
    let start = start?;
    Some(
        text.chars()
            .skip(start.saturating_sub(120))
            .take(length + 240)
            .collect::<String>()
            .trim()
            .to_owned(),
    )
}
/// Shared bounded read for native skill/catalog owners, with the same handle checks.
pub(crate) fn scoped_bytes(root: &Path, relative: &Path, limit: u64) -> Result<Vec<u8>> {
    let root = root.canonicalize().map_err(|_| unavailable())?;
    let scope = Scope {
        key: "native".into(),
        root,
    };
    let path = scope.target(relative.to_str().ok_or_else(denied)?)?;
    let mut file = scope.open(&path)?;
    let before = file.metadata().map_err(|_| unavailable())?;
    if before.len() > limit {
        return Err(fail(
            "FILESYSTEM_TEXT_LIMIT",
            "Файл превышает допустимый размер.",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    let after = file.metadata().map_err(|_| unavailable())?;
    if bytes.len() as u64 > limit
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(fail(
            "FILESYSTEM_CHANGED",
            "Файл изменился во время чтения.",
        ));
    }
    Ok(bytes)
}

pub fn execute(request: &Value) -> Value {
    match run(request) {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(e) => json!({"status":"failed","code":e.code,"detail":e.detail}),
    }
}
fn run(request: &Value) -> Result<Value> {
    if request["schema"] != "les.filesystem.v1" {
        return Err(denied());
    }
    let roots = roots(request)?;
    let action = request["action"].as_str().ok_or_else(denied)?;
    let args = &request["arguments"];
    let mut result = json!({"status":"ok","operation":action,"result":{},"sources":[],"missing":[],"warnings":[]});
    if action == "roots" {
        result["operation"] = json!("list_roots");
        let mut forbidden = FORBIDDEN.to_vec();
        forbidden.sort();
        result["result"] = json!({"roots":roots.iter().map(|(key,path)|json!({"key":key,"path":path,"exists":path.exists(),"read_only":true})).collect::<Vec<_>>(),"forbidden_parts":forbidden});
        return Ok(result);
    }
    if !matches!(action, "list" | "stat" | "read_text" | "search" | "hash") {
        return Err(denied());
    }
    let key = args["root"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("docs");
    let root = roots.get(key).ok_or_else(denied)?;
    let root = if root.exists() {
        root.canonicalize().map_err(|_| unavailable())?
    } else {
        root.clone()
    };
    if forbidden(&root) {
        return Err(denied());
    }
    let scope = Scope {
        key: key.into(),
        root,
    };
    let path = scope.target(args["path"].as_str().unwrap_or(""))?;
    if !path.try_exists().map_err(|_| unavailable())? {
        result["status"] = json!("missing");
        result["missing"] = json!(["path not found"]);
        return Ok(result);
    }
    result["sources"] = json!([scope.source(&path)?]);
    let deadline = Instant::now() + Duration::from_secs(30);
    let value = match action {
        "list" => scope.tree(&path, number(&args["depth"], 1, 0, 4), &mut 2000)?,
        "stat" => scope.metadata(&path)?,
        "read_text" => {
            if !path.is_file() || !supported(&path) {
                result["status"] = json!("missing");
                result["missing"] = json!(["not a supported text file"]);
                return Ok(result);
            }
            let (text, replaced, source_sha256) = scope.text(&path, 1_000_000)?;
            let limit = number(&args["max_chars"], 20000, 200, 100000) as usize;
            if replaced {
                result["warnings"] = json!([
                    "Файл содержит байты вне UTF-8; неподтверждённые символы заменены меткой �."
                ]);
            }
            let mut value = scope.metadata(&path)?;
            value["source_sha256"] = json!(source_sha256);
            value["text_truncated"] = json!(text.chars().count() > limit);
            value["text"] = json!(text.chars().take(limit).collect::<String>());
            value
        }
        "hash" => {
            let mut file = scope.open(&path)?;
            let before = file.metadata().map_err(|_| unavailable())?;
            let mut hash = Sha256::new();
            let mut bytes = [0; 128 * 1024];
            loop {
                if Instant::now() > deadline {
                    return Err(fail("FILESYSTEM_TIMEOUT","Чтение диска не завершилось вовремя. Выберите меньшую папку или проверьте диск."));
                }
                let n = file.read(&mut bytes).map_err(|_| unavailable())?;
                if n == 0 {
                    break;
                }
                hash.update(&bytes[..n]);
            }
            let after = file.metadata().map_err(|_| unavailable())?;
            if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
                return Err(fail(
                    "FILESYSTEM_CHANGED",
                    "Файл изменился во время чтения. Повторите запрос.",
                ));
            }
            let mut value = scope.metadata(&path)?;
            value["sha256"] = json!(format!("{:x}", hash.finalize()));
            value
        }
        "search" => {
            let query = args["q"].as_str().unwrap_or("").trim();
            if query.is_empty() || query.len() > 16384 {
                return Err(fail(
                    "FILESYSTEM_QUERY_INVALID",
                    "Укажите поисковую строку.",
                ));
            }
            let needle = crate::unicode::casefold(query);
            let content = args["content"] == true;
            let limit = number(&args["limit"], 50, 1, 200) as usize;
            let mut stack = vec![path.clone()];
            let mut hits = Vec::new();
            let mut visited = HashSet::new();
            let mut excluded = 0;
            while !stack.is_empty() {
                if hits.len() >= limit || visited.len() >= 5000 {
                    break;
                }
                let current = stack.pop().unwrap();
                if Instant::now() > deadline {
                    return Err(fail(
                        "FILESYSTEM_TIMEOUT",
                        "Поиск по папке не завершился вовремя. Выберите меньшую папку.",
                    ));
                }
                if !visited.insert(current.clone()) {
                    continue;
                }
                let Ok(mut meta) = scope.metadata(&current) else {
                    continue;
                };
                let name = crate::unicode::casefold(
                    current.file_name().and_then(|v| v.to_str()).unwrap_or(""),
                );
                let snippet = if content && current.is_file() && supported(&current) {
                    scope
                        .text(&current, 200000)
                        .ok()
                        .and_then(|(text, _, _)| excerpt(&text, &needle, query.chars().count()))
                } else {
                    None
                };
                if name.contains(&needle) || snippet.is_some() {
                    meta["match"] = json!(if snippet.is_some() { "content" } else { "name" });
                    if let Some(snippet) = snippet {
                        meta["snippet"] = json!(snippet);
                    }
                    hits.push(meta);
                }
                if current.is_dir() {
                    let (mut children, skipped) = scope.children(&current)?;
                    excluded += skipped;
                    children.reverse();
                    stack.extend(children);
                }
            }
            json!({"query":query,"count":hits.len(),"hits":hits,"content":content,"scan_truncated":!stack.is_empty(),"excluded_entries":excluded})
        }
        _ => unreachable!(),
    };
    result["result"] = value;
    if action == "search" && result["result"]["count"] == 0 {
        result["status"] = json!("missing");
        result["missing"] = json!(["no files matched query"]);
    }
    Ok(result)
}
