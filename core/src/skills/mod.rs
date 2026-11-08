//! Offline installed-skill library and approved legacy Python functions.
mod process;
pub mod runtime;
mod worker;
use crate::workspace::{Failure, Result};
pub use process::serve_once;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn fail(code: &'static str, message: &str) -> Failure {
    Failure::new(code, message)
}
fn invalid() -> Failure {
    fail("SKILL_ARGUMENT_INVALID", "Некорректный запрос навыка.")
}
fn storage() -> Failure {
    fail(
        "SKILL_STORAGE_FAILED",
        "Не удалось сохранить или прочитать задание навыка.",
    )
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key].as_str().ok_or_else(invalid)
}
fn absolute(value: &Value, key: &str) -> Result<PathBuf> {
    let path = PathBuf::from(text(value, key)?);
    if !path.is_absolute() {
        return Err(invalid());
    }
    Ok(path)
}
fn read_bytes(root: &Path, relative: &Path, limit: u64) -> Result<Vec<u8>> {
    crate::filesystem::scoped_bytes(root, relative, limit)
}
fn read_json(root: &Path, name: &str, limit: u64) -> Result<Value> {
    serde_json::from_slice(&read_bytes(root, Path::new(name), limit)?).map_err(|_| storage())
}
fn skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 101
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}
fn target(root: &Path, skill: &str, path: &str) -> Result<PathBuf> {
    if !skill_name(skill) {
        return Err(fail("SKILL_NAME_INVALID", "Некорректное имя навыка"));
    }
    let relative = Path::new(path);
    if relative.is_absolute() || path.contains([':', '\0']) {
        return Err(fail(
            "SKILL_PATH_DENIED",
            "Нужен относительный путь внутри библиотеки навыков",
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|_| fail("SKILL_NOT_FOUND", "Навык или файл не найден"))?;
    let package = root
        .join(skill)
        .canonicalize()
        .map_err(|_| fail("SKILL_NOT_FOUND", "Навык или файл не найден"))?;
    let file = package
        .join(relative)
        .canonicalize()
        .map_err(|_| fail("SKILL_NOT_FOUND", "Навык или файл не найден"))?;
    if !package.starts_with(&root) || !file.starts_with(&root) {
        return Err(fail(
            "SKILL_PATH_DENIED",
            "Файл находится вне библиотеки навыков",
        ));
    }
    if !package.join("SKILL.md").is_file() || !file.is_file() {
        return Err(fail("SKILL_NOT_FOUND", "Навык или файл не найден"));
    }
    let relative = file.strip_prefix(&root).map_err(|_| invalid())?;
    if relative
        .components()
        .any(|p| p.as_os_str().to_str().is_none_or(|v| v.starts_with('.')))
    {
        return Err(fail("SKILL_PATH_DENIED", "Скрытые файлы недоступны"));
    }
    Ok(relative.to_path_buf())
}
fn utf8(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes)
        .map(|v| {
            v.strip_prefix('\u{feff}')
                .unwrap_or(&v)
                .replace("\r\n", "\n")
                .replace('\r', "\n")
        })
        .map_err(|_| {
            fail(
                "SKILL_ENCODING_INVALID",
                "Файл навыка не является текстом UTF-8.",
            )
        })
}
fn offset(request: &Value) -> usize {
    request["offset"]
        .as_u64()
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize
}
fn catalog(root: &Path, request: &Value) -> Result<Value> {
    let mut skills = Vec::new();
    if root.exists() {
        let entries = fs::read_dir(root).map_err(|_| storage())?;
        let mut names = Vec::new();
        for entry in entries.take(10001) {
            names.push(entry.map_err(|_| storage())?.file_name());
        }
        if names.len() > 10000 {
            return Err(fail(
                "SKILL_LIBRARY_LIMIT",
                "В библиотеке слишком много навыков.",
            ));
        }
        names.sort();
        for name in names {
            let Some(name) = name.to_str().filter(|s| skill_name(s)) else {
                continue;
            };
            let Ok(path) = target(root, name, "SKILL.md") else {
                continue;
            };
            let Ok(bytes) = read_bytes(root, &path, 1_000_000) else {
                continue;
            };
            let Ok(text) = utf8(bytes) else {
                continue;
            };
            let description = text
                .lines()
                .find_map(|line| line.strip_prefix("description:"))
                .unwrap_or("")
                .trim()
                .chars()
                .take(500)
                .collect::<String>();
            skills.push(json!({"skill":name,"description":description}));
        }
    }
    let total = skills.len();
    let offset = offset(request).min(total);
    let page = &skills[offset..(offset + 10).min(total)];
    Ok(
        json!({"skills":page,"total":total,"next_offset":if offset + page.len() < total {json!(offset+page.len())} else {Value::Null}}),
    )
}
fn read(root: &Path, request: &Value) -> Result<Value> {
    let skill = text(request, "skill")?;
    let path = target(root, skill, request["path"].as_str().unwrap_or("SKILL.md"))?;
    if !matches!(
        path.extension()
            .and_then(|v| v.to_str())
            .map(str::to_lowercase)
            .as_deref(),
        Some("md" | "txt" | "py" | "json" | "yaml" | "yml" | "csv")
    ) {
        return Err(fail(
            "SKILL_TEXT_TYPE_INVALID",
            "Этот файл нельзя прочитать как текст",
        ));
    }
    let text = utf8(read_bytes(root, &path, 1_000_000)?)?;
    let total = text.chars().count();
    let offset = offset(request).min(total);
    let chunk = text.chars().skip(offset).take(6000).collect::<String>();
    let next = offset + chunk.chars().count();
    Ok(
        json!({"skill":skill,"path":path.to_string_lossy().replace('\\',"/"),"text":chunk,"total_chars":total,"offset":offset,
        "next_offset":if next < total {json!(next)} else {Value::Null},"arbitrary_code_execution":false}),
    )
}
fn approvals(root: &Path) -> Result<Value> {
    if !root
        .join("approved-compute.json")
        .try_exists()
        .map_err(|_| storage())?
    {
        return Ok(json!([]));
    }
    let value = read_json(root, "approved-compute.json", 1_000_000)?;
    if !value.as_array().is_some_and(|v| v.len() <= 4096) {
        return Err(fail(
            "SKILL_APPROVAL_INVALID",
            "Не удалось прочитать список разрешённых скриптов.",
        ));
    }
    Ok(value)
}
fn approve(root: &Path, skill: &str, path: &str) -> Result<(Value, Vec<u8>)> {
    let approved = approvals(root)?
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["skill"] == skill && v["path"] == path)
        .cloned()
        .ok_or_else(|| fail("SKILL_NOT_APPROVED", "Скрипт не разрешён для вычислений"))?;
    let source = read_bytes(root, &target(root, skill, path)?, 4 * 1024 * 1024)?;
    if approved["sha256"] != crate::sha256_hex(&source) {
        return Err(fail(
            "SKILL_SOURCE_CHANGED",
            "Скрипт изменён после проверки. Выполнение остановлено",
        ));
    }
    let entry = text(&approved, "entrypoint")?;
    if entry.is_empty()
        || !entry.as_bytes()[0].is_ascii_alphabetic()
        || !entry
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || v == b'_')
    {
        return Err(fail("SKILL_ENTRYPOINT_INVALID", "Некорректная точка входа"));
    }
    Ok((approved, source))
}
fn validate_items(value: &Value) -> Result<&Vec<Value>> {
    let items = value
        .as_array()
        .filter(|v| (1..=100).contains(&v.len()))
        .ok_or_else(|| fail("SKILL_ITEMS_INVALID", "Передайте от 1 до 100 позиций"))?;
    let mut seen = HashSet::new();
    for item in items {
        let id = item["id"]
            .as_str()
            .filter(|s| (1..=100).contains(&s.chars().count()) && seen.insert(*s))
            .ok_or_else(|| {
                fail(
                    "SKILL_ITEM_ID_INVALID",
                    "Для каждой позиции нужен уникальный строковый ID",
                )
            })?;
        let _ = id;
        if !item["input"].is_object() {
            return Err(fail(
                "SKILL_ITEM_INPUT_INVALID",
                "Входные данные каждой позиции должны быть объектом JSON",
            ));
        }
    }
    Ok(items)
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let pending = path.with_extension("pending");
    let mut file = File::create(&pending).map_err(|_| storage())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| storage())?;
    drop(file);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match fs::rename(&pending, path) {
            Ok(()) => return Ok(()),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return Err(storage()),
        }
    }
}
fn json_file(path: &Path, value: &Value) -> Result<()> {
    atomic(path, &serde_json::to_vec(value).map_err(|_| storage())?)
}
fn job_path(request: &Value, id: &str) -> Result<PathBuf> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(fail("SKILL_JOB_ID_INVALID", "Некорректный номер расчёта"));
    }
    let root = absolute(request, "artifacts")?
        .canonicalize()
        .map_err(|_| fail("SKILL_JOB_NOT_FOUND", "Расчёт не найден"))?;
    let work = root
        .join(id)
        .canonicalize()
        .map_err(|_| fail("SKILL_JOB_NOT_FOUND", "Расчёт не найден"))?;
    if !work.starts_with(root) {
        return Err(fail(
            "SKILL_PATH_DENIED",
            "Расчёт находится вне каталога заданий.",
        ));
    }
    Ok(work)
}
fn saved(work: &Path, input: &Value) -> Result<Vec<Value>> {
    let items = validate_items(&input["items"])?;
    let mut rows = Vec::new();
    let mut missing = false;
    for (index, item) in items.iter().enumerate() {
        let path = format!("{index:04}.json");
        if !work.join(&path).try_exists().map_err(|_| storage())? {
            missing = true;
            continue;
        }
        if missing {
            return Err(fail(
                "SKILL_RESULT_CORRUPT",
                "Нарушен порядок сохранённых строк. Автоматический повтор остановлен.",
            ));
        }
        let row = read_json(work, &path, 128000)?;
        if row["id"] != item["id"] || !matches!(row["status"].as_str(), Some("ok" | "error")) {
            return Err(fail(
                "SKILL_RESULT_CORRUPT",
                "Сохранённая строка не соответствует заданию.",
            ));
        }
        rows.push(row);
    }
    Ok(rows)
}
fn summary(
    work: &Path,
    id: &str,
    input: &Value,
    rows: &[Value],
    reason: Option<&str>,
) -> Result<Value> {
    let items = validate_items(&input["items"])?;
    let interrupted = rows.len() < items.len() || reason.is_some();
    let message=match reason {
        Some("SKILL_DEPENDENCY_MISSING")=>{
            let failure=read_json(work,"failure.json",65536).unwrap_or(Value::Null);
            let name=failure["module"].as_str().filter(|v|!v.is_empty()&&v.len()<=128
                &&v.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'_'||c==b'.'));
            Some(name.map(|v|format!("Внешнему навыку не хватает Python-библиотеки «{v}». Проверьте комплект зависимостей навыка."))
                .unwrap_or_else(||"Внешнему навыку не хватает Python-библиотеки. Проверьте комплект зависимостей навыка.".into()))
        },
        Some("SKILL_RUNTIME_MISSING")=>Some("Не найден исполнитель внешних Python-навыков. Восстановите комплект навыков Леса.".into()),
        Some("SKILL_RUNTIME_INVALID")=>Some("Комплект внешних Python-навыков повреждён. Восстановите его перед продолжением задания.".into()),
        Some("SKILL_DEADLINE_EXCEEDED")=>Some("Время выполнения навыка истекло. Подтверждённые строки сохранены; задание можно продолжить.".into()),
        Some("SKILL_SCRIPT_FAILED")=>Some("Скрипт внешнего навыка завершился с ошибкой. Подробности сохранены в задании.".into()),
        _=>None,
    };
    let summary = json!({"job_id":id,"script_sha256":input["sha256"],"total":items.len(),"completed":rows.len(),
        "succeeded":rows.iter().filter(|v| v["status"]=="ok").count(),"interrupted":interrupted,
        "pending_ids":items[rows.len()..].iter().map(|v| v["id"].clone()).collect::<Vec<_>>(),
        "error_code":reason,"error_message":message});
    json_file(&work.join("summary.json"), &summary)?;
    Ok(summary)
}
fn run(request: &Value, work: &Path, id: &str, input: &Value) -> Result<Value> {
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(work.join("writer.lock"))
        .map_err(|_| storage())?;
    lock.try_lock()
        .map_err(|_| fail("SKILL_JOB_BUSY", "Задание навыка уже выполняется."))?;
    let mut rows = saved(work, input)?;
    if rows.len() < input["items"].as_array().unwrap().len() {
        summary(work, id, input, &rows, Some("SKILL_RUNNING"))?;
        let reason = process::run(request, work, input, &mut rows)?;
        let mut value = summary(work, id, input, &rows, reason.as_deref())?;
        value["rows"] = json!(rows.iter().take(10).collect::<Vec<_>>());
        value["next_offset"] = if rows.len() > 10 {
            json!(10)
        } else {
            Value::Null
        };
        return Ok(value);
    }
    let mut value = summary(work, id, input, &rows, None)?;
    value["rows"] = json!(rows.iter().take(10).collect::<Vec<_>>());
    value["next_offset"] = if rows.len() > 10 {
        json!(10)
    } else {
        Value::Null
    };
    Ok(value)
}
fn calculate(request: &Value, root: &Path) -> Result<Value> {
    let (approved, source) = approve(root, text(request, "skill")?, text(request, "path")?)?;
    validate_items(&request["items"])?;
    let input = json!({"sha256":approved["sha256"],"entrypoint":approved["entrypoint"],"items":request["items"],"skill":request["skill"],"path":request["path"]});
    let encoded = serde_json::to_vec(&input).map_err(|_| invalid())?;
    if encoded.len() > 256000 {
        return Err(fail("SKILL_INPUT_LIMIT", "Входные данные превышают 256 КБ"));
    }
    let artifacts = absolute(request, "artifacts")?;
    fs::create_dir_all(&artifacts).map_err(|_| storage())?;
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| storage())?;
    let id = bytes.iter().map(|v| format!("{v:02x}")).collect::<String>();
    let work = artifacts.join(&id);
    fs::create_dir(&work).map_err(|_| storage())?;
    atomic(&work.join("script.py"), &source)?;
    atomic(&work.join("input.json"), &encoded)?;
    run(request, &work, &id, &input)
}
fn read_job(request: &Value) -> Result<Value> {
    let id = text(request, "job_id")?;
    let work = job_path(request, id)?;
    let input = read_json(&work, "input.json", 256000)?;
    let rows = saved(&work, &input)?;
    let offset = offset(request).min(rows.len());
    let page = &rows[offset..(offset + 10).min(rows.len())];
    Ok(
        json!({"job_id":id,"rows":page,"saved_rows":rows.len(),"next_offset":if offset+page.len()<rows.len(){json!(offset+page.len())}else{Value::Null}}),
    )
}
pub fn execute(request: &Value) -> Value {
    let action = || -> Result<Value> {
        if request["schema"] != "les.skills.v1" {
            return Err(invalid());
        }
        if request["action"] == "read_job" {
            return read_job(request);
        }
        if request["action"] == "runtime" {
            let root = absolute(request, "root")?;
            let application = absolute(request, "application")?;
            let program = runtime::select(&root, &application, None)?;
            return Ok(json!({"program":program,"separate":program!=application}));
        }
        let root = absolute(request, "library")?;
        match text(request, "action")? {
            "catalog" => catalog(&root, request),
            "read" => read(&root, request),
            "approvals" => Ok(json!({"scripts":approvals(&root)?})),
            "calculate" => calculate(request, &root),
            "resume" => {
                let id = text(request, "job_id")?;
                let work = job_path(request, id)?;
                let input = read_json(&work, "input.json", 256000)?;
                let (approved, _) = approve(&root, text(&input, "skill")?, text(&input, "path")?)?;
                if input["sha256"] != approved["sha256"]
                    || input["entrypoint"] != approved["entrypoint"]
                    || crate::sha256_hex(&read_bytes(
                        &work,
                        Path::new("script.py"),
                        4 * 1024 * 1024,
                    )?) != approved["sha256"]
                {
                    return Err(fail(
                        "SKILL_SOURCE_CHANGED",
                        "Скрипт изменён после проверки. Выполнение остановлено",
                    ));
                }
                run(request, &work, id, &input)
            }
            _ => Err(invalid()),
        }
    };
    match action() {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => json!({"ok":false,"error":{"code":error.code,"message":error.detail}}),
    }
}
