use super::*;

pub(super) fn lease(base: &Path) -> Result<File> {
    let path = base.join("lock");
    paths::regular(&path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| unavailable())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(failure(
                    "AGENT_BUSY",
                    "Рабочие файлы заняты другим изменением. Повторите сохранение.",
                ))
            }
            Err(_) => return Err(unavailable()),
        }
    }
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    paths::regular(path)?;
    let temp = path.with_file_name(format!("pending-{}.tmp", identifier()?));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|_| unavailable())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| unavailable())?;
    drop(file);
    if let Err(_) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(unavailable());
    }
    #[cfg(unix)]
    {
        File::open(path.parent().unwrap())
            .and_then(|file| file.sync_all())
            .map_err(|_| unavailable())?;
    }
    Ok(())
}
fn history_path(base: &Path, relative: &str, id: &str, create: bool) -> Result<PathBuf> {
    if id.is_empty() || id.len() > 80 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(failure("AGENT_INVALID", "Неизвестная версия."));
    }
    paths::target(
        base,
        &format!("history/{}/{id}.json", digest(relative.as_bytes())),
        create,
    )
}
fn read_record(base: &Path, relative: &str, id: &str) -> Result<Value> {
    let path = history_path(base, relative, id, false)?;
    let bytes = read(&path, 16 * FILE_LIMIT)?
        .ok_or_else(|| failure("AGENT_NOT_FOUND", "Версия не найдена."))?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "AGENT_HISTORY_INVALID",
            "История изменения повреждена. Текущий файл сохранён.",
        )
    })?;
    if record["path"] != relative || record["id"] != id {
        return Err(failure(
            "AGENT_HISTORY_INVALID",
            "Версия принадлежит другому файлу.",
        ));
    }
    Ok(record)
}
fn pending_path(base: &Path) -> PathBuf {
    base.join("pending.json")
}
fn record_allowed(relative: &str) -> bool {
    if paths::validate_relative(relative).is_err() {
        return false;
    }
    if ["USER.md", "MEMORY.md", "policy.json"].contains(&relative) {
        return true;
    }
    let parts = relative.split('/').collect::<Vec<_>>();
    matches!(parts.as_slice(),["projects",id,"MEMORY.md"] if id.parse::<u64>().is_ok_and(|v|v>0))
        || matches!(parts.as_slice(),["workspaces",id,tail @ ..] if id.parse::<u64>().is_ok()&&!tail.is_empty())
}
pub(super) fn recover(base: &Path) -> Result<()> {
    let path = pending_path(base);
    let Some(bytes) = read(&path, 16 * FILE_LIMIT)? else {
        return Ok(());
    };
    let mut record: Value = serde_json::from_slice(&bytes).map_err(|_| {
        failure(
            "AGENT_RECOVERY_REQUIRED",
            "Журнал изменения повреждён. Рабочие файлы сохранены; откройте диагностику.",
        )
    })?;
    let relative = record["path"]
        .as_str()
        .filter(|r| record_allowed(r))
        .ok_or_else(|| {
            failure(
                "AGENT_RECOVERY_REQUIRED",
                "Нельзя восстановить запись вне рабочего каталога.",
            )
        })?
        .to_string();
    let id = record["id"].as_str().ok_or_else(unavailable)?.to_string();
    let target = paths::target(base, &relative, true)?;
    let current = read(&target, FILE_LIMIT)?;
    let current_revision = revision(current.as_deref());
    let next = record["text"].as_str().ok_or_else(unavailable)?;
    let exists = record["exists"].as_bool().ok_or_else(unavailable)?;
    let next_revision = if exists {
        digest(next.as_bytes())
    } else {
        "missing".into()
    };
    if next.len() > FILE_LIMIT || record["revision"] != next_revision {
        return Err(failure(
            "AGENT_RECOVERY_REQUIRED",
            "Не совпала контрольная сумма восстанавливаемого файла.",
        ));
    }
    if current_revision == record["previous_revision"] {
        publish(&target, next.as_bytes(), exists)?;
    } else if current_revision != record["revision"] {
        // An external editor wins. Preserve the abandoned intent for inspection.
        record["status"] = "conflict".into();
        record["observed_revision"] = current_revision.into();
    }
    let history = history_path(base, &relative, &id, true)?;
    atomic(
        &history,
        &serde_json::to_vec(&record).map_err(|_| unavailable())?,
    )?;
    sync_removed(base, &record)?;
    fs::remove_file(path).map_err(|_| unavailable())?;
    Ok(())
}
pub(super) fn change(base: &Path, action: &str, args: &Map<String, Value>) -> Result<Value> {
    change_with_hook(base, action, args, |_| {})
}
pub(super) fn change_with_hook(
    base: &Path,
    action: &str,
    args: &Map<String, Value>,
    hook: impl Fn(&str),
) -> Result<Value> {
    let relative = if action == "policy_save" {
        "policy.json".into()
    } else {
        paths::relative(args)?
    };
    let target = paths::target(base, &relative, true)?;
    let limit = if args.get("area") == Some(&json!("files")) {
        FILE_LIMIT
    } else {
        MEMORY_LIMIT
    };
    let previous = read(&target, limit)?;
    let previous_revision = revision(previous.as_deref());
    if text(args, "expected_revision")? != previous_revision {
        return Err(failure("AGENT_CONFLICT","Файл изменён в другом окне или редакторе. Ваш текст сохранён здесь; сравните версии перед сохранением."));
    }
    let mut next_exists = true;
    let next = if action == "delete" {
        next_exists = false;
        String::new()
    } else if action == "restore" {
        if !matches!(
            args.get("side").and_then(Value::as_str),
            Some("before" | "after")
        ) {
            return Err(failure(
                "AGENT_INVALID",
                "Для restore передайте side: before или after.",
            ));
        }
        let record = read_record(base, &relative, text(args, "history_id")?)?;
        let before = args.get("side").and_then(Value::as_str) == Some("before");
        next_exists = record[if before { "before_exists" } else { "exists" }]
            .as_bool()
            .ok_or_else(unavailable)?;
        record[if before { "before" } else { "text" }]
            .as_str()
            .ok_or_else(unavailable)?
            .to_string()
    } else if action == "edit" {
        let old = text(args, "old_text")?;
        let new = text(args, "new_text")?;
        let source = content(previous.as_deref())?;
        if old.is_empty() || source.matches(old).count() != 1 {
            return Err(failure("AGENT_EDIT_AMBIGUOUS","Фрагмент для замены не найден или встречается несколько раз. Перечитайте файл и выберите точный фрагмент."));
        }
        source.replacen(old, new, 1)
    } else if action == "policy_save" {
        let value = args.get("value").ok_or_else(unavailable)?;
        if value.as_object().is_none_or(|v| v.len() != 2)
            || !value["auto_memory"].is_boolean()
            || !value["workspace_write"].is_boolean()
        {
            return Err(failure(
                "AGENT_INVALID",
                "Проверьте настройки автоматических изменений.",
            ));
        }
        value.to_string()
    } else {
        text(args, "text")?.to_string()
    };
    if next.len() > limit || next.contains('\0') {
        return Err(failure(
            "AGENT_TOO_LARGE",
            "Текст слишком велик или содержит двоичные данные. Сохраните его отдельным файлом.",
        ));
    }
    if (next_exists && previous.as_deref() == Some(next.as_bytes()))
        || (!next_exists && previous.is_none())
    {
        return Ok(
            json!({"revision":previous_revision,"persisted":true,"changed":false,"path":relative}),
        );
    }
    // Preserve the previous UTF-8 bytes in history; invalid encodings are never overwritten.
    content(previous.as_deref())?;
    let before =
        std::str::from_utf8(previous.as_deref().unwrap_or_default()).map_err(|_| unavailable())?;
    let id = format!(
        "{:020}-{}",
        chrono::Utc::now().timestamp_micros(),
        identifier()?
    );
    let actor = args
        .get("actor")
        .and_then(Value::as_str)
        .filter(|a| ["user", "agent"].contains(a))
        .unwrap_or("user");
    let reason = args.get("reason").and_then(Value::as_str).unwrap_or("");
    if reason.chars().count() > 500 {
        return Err(failure(
            "AGENT_INVALID",
            "Сократите описание изменения до 500 символов.",
        ));
    }
    let record = json!({"schema":"les.agent-file-change.v1","id":id,"path":relative,"previous_revision":previous_revision,
        "revision":if next_exists{digest(next.as_bytes())}else{"missing".into()},"before":before,"before_exists":previous.is_some(),"text":next,"exists":next_exists,"actor":actor,"reason":reason,
        "at":chrono::Utc::now().to_rfc3339(),"status":"committed"});
    atomic(
        &pending_path(base),
        &serde_json::to_vec(&record).map_err(|_| unavailable())?,
    )?;
    hook("pending");
    // Recheck after durable intent: external edits must not be overwritten.
    let current = read(&target, limit)?;
    if revision(current.as_deref()) != previous_revision {
        recover(base)?;
        return Err(failure(
            "AGENT_CONFLICT",
            "Внешний редактор изменил файл. Правки сохранены в истории конфликта.",
        ));
    }
    publish(&target, next.as_bytes(), next_exists)?;
    hook("target");
    let history = history_path(base, &relative, &id, true)?;
    atomic(
        &history,
        &serde_json::to_vec(&record).map_err(|_| unavailable())?,
    )?;
    hook("history");
    sync_removed(base, &record)?;
    fs::remove_file(pending_path(base)).map_err(|_| unavailable())?;
    Ok(
        json!({"revision":record["revision"],"persisted":true,"changed":true,"path":relative,"history_id":id}),
    )
}
fn publish(target: &Path, bytes: &[u8], exists: bool) -> Result<()> {
    if exists {
        atomic(target, bytes)
    } else {
        if paths::regular(target)? {
            fs::remove_file(target).map_err(|_| unavailable())?;
        }
        Ok(())
    }
}
// Small per-project tombstones make deleted files discoverable without scanning
// every project's full history. The pending intent also recovers this index.
fn sync_removed(base: &Path, record: &Value) -> Result<()> {
    let relative = record["path"].as_str().ok_or_else(unavailable)?;
    let Some((project, _)) = relative
        .strip_prefix("workspaces/")
        .and_then(|r| r.split_once('/'))
    else {
        return Ok(());
    };
    let target = paths::target(base, relative, false)?;
    let exists = paths::regular(&target)?;
    let marker = paths::target(
        base,
        &format!("removed/{project}/{}.json", digest(relative.as_bytes())),
        !exists,
    )?;
    if exists {
        if paths::regular(&marker)? {
            fs::remove_file(marker).map_err(|_| unavailable())?;
        }
    } else {
        atomic(
            &marker,
            &serde_json::to_vec(&json!({"path":relative,"history_id":record["id"]}))
                .map_err(|_| unavailable())?,
        )?;
    }
    Ok(())
}
pub(super) fn history(base: Option<&Path>, relative: &str, offset: usize) -> Result<Value> {
    let Some(base) = base else {
        return Ok(json!({"items":[]}));
    };
    let folder = base.join("history").join(digest(relative.as_bytes()));
    if !paths::regular(&base.join("history"))? || !paths::regular(&folder)? {
        return Ok(json!({"items":[]}));
    }
    let mut names = fs::read_dir(folder)
        .map_err(|_| unavailable())?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|_| unavailable())?;
    names.sort_by(|a, b| b.cmp(a));
    let more = names.len() > offset.saturating_add(50);
    let mut items = Vec::new();
    for name in names.into_iter().skip(offset).take(50) {
        let name = name.to_str().ok_or_else(unavailable)?;
        let id = name.strip_suffix(".json").ok_or_else(unavailable)?;
        let record = read_record(base, relative, id)?;
        items.push(json!({"id":record["id"],"at":record["at"],"actor":record["actor"],"reason":record["reason"],"revision":record["revision"],"status":record["status"],"exists":record["exists"],"before_exists":record["before_exists"]}));
    }
    Ok(json!({"items":items,"more":more,"next_offset":if more{Some(offset+50)}else{None}}))
}
pub(super) fn version(base: Option<&Path>, relative: &str, id: &str) -> Result<Value> {
    read_record(
        base.ok_or_else(|| failure("AGENT_NOT_FOUND", "Версия не найдена."))?,
        relative,
        id,
    )
}
pub(super) fn list(base: Option<&Path>, project: i64, include_deleted: bool) -> Result<Value> {
    let Some(base) = base else {
        return Ok(json!({"files":[],"skipped":0}));
    };
    let root = base.join("workspaces").join(project.to_string());
    let mut stack = if paths::regular(&base.join("workspaces"))? && paths::regular(&root)? {
        vec![root.clone()]
    } else {
        vec![]
    };
    let mut files = Vec::new();
    let mut skipped = 0;
    while let Some(folder) = stack.pop() {
        for entry in fs::read_dir(folder).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let path = entry.path();
            if paths::regular(&path).is_err() {
                skipped += 1;
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| unavailable())?
                .to_str()
                .ok_or_else(unavailable)?
                .replace('\\', "/");
            if paths::validate_relative(&relative).is_err() {
                skipped += 1;
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(json!({"path":relative,"bytes":entry.metadata().map_err(|_|unavailable())?.len(),"exists":true}));
            }
            if files.len() + stack.len() > 500 {
                return Err(failure(
                    "AGENT_TOO_LARGE",
                    "В рабочем каталоге слишком много файлов для одного списка.",
                ));
            }
        }
    }
    if include_deleted {
        let folder = base.join("removed").join(project.to_string());
        if paths::regular(&base.join("removed"))? && paths::regular(&folder)? {
            for entry in fs::read_dir(folder).map_err(|_| unavailable())? {
                let path = entry.map_err(|_| unavailable())?.path();
                let bytes = read(&path, 4096)?.ok_or_else(unavailable)?;
                let marker: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
                let relative = marker["path"]
                    .as_str()
                    .and_then(|p| p.strip_prefix(&format!("workspaces/{project}/")))
                    .ok_or_else(unavailable)?;
                paths::validate_relative(relative)?;
                if !files.iter().any(|f| f["path"] == relative) {
                    files.push(json!({"path":relative,"bytes":0,"exists":false,"history_id":marker["history_id"]}));
                }
                if files.len() > 500 {
                    return Err(failure(
                        "AGENT_TOO_LARGE",
                        "Слишком много рабочих файлов для одного списка.",
                    ));
                }
            }
        }
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(json!({"files":files,"skipped":skipped}))
}
