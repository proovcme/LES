//! Route archived and local originals through the private mail registry.
use super::{Result, BAD};
use crate::Context;
use les_core::catalog::mail;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const IO: &str = "MAIL_SOURCE_UNAVAILABLE";
const INVALID: &str = "MAIL_SOURCE_PATH_INVALID";

fn number(input: &Value, key: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    match input.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (min..=max).contains(n))
            .ok_or(BAD),
    }
}

fn archive_account(context: &Context, source: &Path, kind: &str) -> Result<Value> {
    let key = les_core::sha256_hex(source.to_str().ok_or(INVALID)?.as_bytes());
    let label: String = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Почта")
        .chars()
        .filter(|c| !c.is_control())
        .take(140)
        .collect();
    mail::create(
        &context.state_root,
        &context.database,
        &json!({"kind":"archive","label":label,"native_account_id":format!("{kind}:{key}")}),
        true,
    )
}

pub(super) fn import(context: &Context, input: &Value) -> Result<Value> {
    let raw = input["path"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(BAD)?;
    let path = crate::intake::external_file(&context.state_root, Path::new(raw))?;
    let format = suffix(&path);
    if !matches!(format.as_str(), "olm" | "pst") {
        return Err("MAIL_FORMAT_UNSUPPORTED");
    }
    let limit = number(input, "max_messages", 2000, 1, 50000)?;
    let offset = number(input, "offset", 0, 0, 1_000_000)?;
    let parse = input["parse"].as_bool().unwrap_or(false);
    let file = File::open(&path).map_err(|_| IO)?;
    let mut account = None;
    let mut created = 0;
    let save = |entry: &str, eml: &[u8]| {
        if account.is_none() {
            account = Some(archive_account(context, &path, &format)?);
        }
        let id = account.as_ref().and_then(|v| v["id"].as_str()).ok_or(BAD)?;
        let folder = entry.rsplit_once('/').map_or("", |(dir, _)| dir);
        let native = les_core::sha256_hex(entry.as_bytes());
        let saved = mail::register(
            &context.state_root,
            id,
            "eml",
            eml,
            &json!({
                "native_id":native,"folder_id":folder,"folder_path":folder,"parse":parse
            }),
        )?;
        created += usize::from(saved["duplicate"] == false);
        Ok(())
    };
    let (count, skipped, next, skipped_attachments) = if format == "pst" {
        super::pst::visit(file, offset, limit, save)?
    } else {
        let (count, skipped, next) = mail::visit_olm(file, offset, limit, save)?;
        (count, skipped, next, 0)
    };
    let account = match account {
        Some(account) => account,
        None if offset > 0 => {
            return Ok(
                json!({"status":"registered","format":format,"messages":0,"created":0,
                "skipped":skipped,"skipped_attachments":skipped_attachments,"next_offset":next,"parse_started":false}),
            );
        }
        None => return Err("MAIL_ARCHIVE_EMPTY"),
    };
    crate::intake::wake(context);
    Ok(json!({
        "status":"registered","component":"Е.Ж.И.К.","archive":path.file_name().and_then(|v|v.to_str()),
        "format":format,"dataset_id":account["dataset_id"],"dataset_name":account["dataset_name"],
        "account_id":account["id"],"messages":count,"created":created,"skipped":skipped,
        "skipped_attachments":skipped_attachments,"next_offset":next,
        "parse_started":parse,"parse_blocked":false,"parse_result":null
    }))
}

fn suffix(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn local_root(context: &Context, raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() || path.starts_with("~") {
        return crate::intake::attachment_folder(&context.state_root, raw);
    }
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(INVALID);
    }
    let base = context
        .state_root
        .join("RAG_Content")
        .canonicalize()
        .map_err(|_| IO)?;
    let path = base
        .join(path.strip_prefix("RAG_Content").unwrap_or(path))
        .canonicalize()
        .map_err(|_| IO)?;
    if !path.starts_with(base) || !path.is_dir() {
        return Err(INVALID);
    }
    Ok(path)
}

pub(super) fn local(
    context: &Context,
    account: Option<&str>,
    input: &Value,
    apple: bool,
) -> Result<Value> {
    let field = if apple { "mail_root" } else { "source_folder" };
    let default = if apple { "~/Library/Mail" } else { "MAIL" };
    let raw = input[field]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(default);
    let root = local_root(context, raw)?;
    let limit = if apple {
        number(input, "max_messages", 25, 1, 200)?
    } else {
        number(input, "max_files", 500, 1, 5000)?
    };
    let parse = input["parse"].as_bool().unwrap_or(false);
    let mut account = account
        .map(|id| mail::account(&context.state_root, id))
        .transpose()?;
    let mut dirs = vec![root.clone()];
    let mut scanned = std::collections::HashSet::new();
    let mut files = Vec::new();
    let mut visited = 0;
    let start = Instant::now();
    let mut skipped = 0;
    while let Some(dir) = dirs.pop() {
        if start.elapsed() > Duration::from_secs(100) || visited >= 100_000 {
            return Err("MAIL_ARCHIVE_LIMIT");
        }
        let dir = dir.canonicalize().map_err(|_| IO)?;
        if !dir.starts_with(&root) || !scanned.insert(dir.clone()) {
            continue;
        }
        for entry in fs::read_dir(dir).map_err(|_| IO)? {
            visited += 1;
            if visited > 100_000 {
                return Err("MAIL_ARCHIVE_LIMIT");
            }
            let entry = entry.map_err(|_| IO)?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let kind = entry.file_type().map_err(|_| IO)?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                let ext = suffix(&path);
                if (apple && ext == "emlx")
                    || (!apple && matches!(ext.as_str(), "eml" | "emlx" | "msg"))
                {
                    files.push(path);
                }
            }
        }
    }
    files.sort_unstable();
    let truncated = files.len() > limit;
    let mut imported = 0;
    for source in files.into_iter().take(limit) {
        let path = source.canonicalize().map_err(|_| IO)?;
        let relative = path.strip_prefix(&root).map_err(|_| INVALID)?;
        let folder = relative.parent().and_then(Path::to_str).unwrap_or_default();
        let ext = suffix(&path);
        let mut bytes = Vec::new();
        File::open(&path)
            .map_err(|_| IO)?
            .take(mail::MAX_MESSAGE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| IO)?;
        if bytes.is_empty() || bytes.len() > mail::MAX_MESSAGE {
            skipped += 1;
            continue;
        }
        if account.is_none() {
            account = Some(archive_account(
                context,
                &root,
                if apple { "apple" } else { "local" },
            )?);
        }
        let id = account.as_ref().and_then(|v| v["id"].as_str()).ok_or(BAD)?;
        mail::register(
            &context.state_root,
            id,
            &ext,
            &bytes,
            &json!({"folder_id":folder,"folder_path":folder,"parse":parse}),
        )?;
        imported += 1;
    }
    if imported == 0 {
        return Err("MAIL_ARCHIVE_EMPTY");
    }
    let account = account.ok_or(BAD)?;
    crate::intake::wake(context);
    Ok(
        json!({"status":"registered","component":"Е.Ж.И.К.","account_id":account["id"],
        "dataset_id":account["dataset_id"],"dataset_name":account["dataset_name"],"files":imported,
        "messages":imported,"skipped":skipped,"truncated":truncated,"parse_started":parse}),
    )
}
