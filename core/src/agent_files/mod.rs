//! Reversible local agent state. Source documents are never writable targets.
mod paths;
mod projection;
#[cfg(test)]
mod tests;
mod tool;
mod transaction;

use crate::workspace::Failure;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Failure>;
const MEMORY_LIMIT: usize = 131072;
const FILE_LIMIT: usize = 1048576;
fn failure(code: &'static str, message: &str) -> Failure {
    Failure::new(code, message)
}
fn unavailable() -> Failure {
    failure("AGENT_UNAVAILABLE","Не удалось открыть рабочие файлы Леса. Проверьте доступ к папке данных; ваш текст сохранён в этом окне.")
}
fn text<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| failure("AGENT_INVALID", &format!("Не заполнено строковое поле {key}. Для записи передайте text и expected_revision из результата read; для нового файла ревизия — missing.")))
}
fn project(args: &Map<String, Value>) -> Result<i64> {
    args.get("project_id")
        .and_then(Value::as_i64)
        .filter(|v| *v >= 0)
        .ok_or_else(|| failure("AGENT_INVALID", "Выберите проект или общую память."))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn identifier() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| unavailable())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>> {
    if !paths::regular(path)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| unavailable())?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > limit {
        return Err(failure(
            "AGENT_TOO_LARGE",
            "Файл слишком велик. Разделите его на несколько рабочих файлов.",
        ));
    }
    Ok(Some(bytes))
}
fn content(bytes: Option<&[u8]>) -> Result<String> {
    let bytes = bytes.unwrap_or_default();
    let bytes = bytes.strip_prefix(&[239, 187, 191]).unwrap_or(bytes);
    std::str::from_utf8(bytes).map(str::to_string).map_err(|_| {
        failure(
            "AGENT_ENCODING",
            "Не удалось прочитать UTF-8. Сохраните файл в UTF-8; исходный файл не изменён.",
        )
    })
}
fn revision(bytes: Option<&[u8]>) -> String {
    bytes.map(digest).unwrap_or_else(|| "missing".into())
}
fn policy(base: Option<&Path>) -> Result<Value> {
    let default = json!({"auto_memory":true,"workspace_write":true});
    let Some(base) = base else {
        return Ok(default);
    };
    let path = paths::target(base, "policy.json", false)?;
    match read(&path, 4096)? {
        None => Ok(default),
        Some(bytes) => {
            let value:Value=serde_json::from_slice(&bytes).map_err(|_|failure("AGENT_POLICY_INVALID","Не удалось прочитать настройки агента. Откройте память и сохраните настройки заново."))?;
            if !value["auto_memory"].is_boolean() || !value["workspace_write"].is_boolean() {
                return Err(failure(
                    "AGENT_POLICY_INVALID",
                    "Настройки агента повреждены. Откройте память Леса.",
                ));
            }
            Ok(value)
        }
    }
}
fn snapshot(base: Option<&Path>, args: &Map<String, Value>) -> Result<Value> {
    let relative = paths::relative(args)?;
    let limit = if args["area"] == "files" {
        FILE_LIMIT
    } else {
        MEMORY_LIMIT
    };
    let bytes = match base {
        Some(base) => read(&paths::target(base, &relative, false)?, limit)?,
        None => None,
    };
    Ok(
        json!({"area":args["area"],"project_id":if args["area"]=="user"{0}else{project(args)?},"path":relative,
        "revision":revision(bytes.as_deref()),"exists":bytes.is_some(),"text":content(bytes.as_deref())?,"is_evidence":false}),
    )
}
pub fn execute(request: &Value) -> Value {
    match dispatch(request) {
        Ok(value) => json!({"status":"completed","value":value}),
        Err(e) => json!({"status":"failed","code":e.code,"detail":e.detail}),
    }
}
fn dispatch(request: &Value) -> Result<Value> {
    if request["schema"] != "les.agent-files.v1" {
        return Err(failure(
            "AGENT_INVALID",
            "Неверный запрос к рабочим файлам.",
        ));
    }
    let root = Path::new(request["state_root"].as_str().ok_or_else(unavailable)?);
    let args = request["arguments"].as_object().ok_or_else(unavailable)?;
    let action = request["action"].as_str().ok_or_else(unavailable)?;
    match action {
        "tool_contract" => return Ok(tool::contract()),
        "tool_binding" => return tool::binding(request),
        "tool_execute" => return tool::execute(request),
        "tool_request" => return tool::request_context(args),
        _ => {}
    }
    if matches!(action, "save" | "edit" | "delete") && args.get("area") == Some(&json!("policy")) {
        return Err(failure(
            "AGENT_INVALID",
            "Для настроек используйте форму управления памятью.",
        ));
    }
    let id = project(args)?;
    if id > 0 {
        let value = crate::catalog::execute(
            &json!({"schema":"les.catalog.v1","database":request["database"],"action":"project_get","arguments":{"project_id":id}}),
        );
        if value["status"] != "completed" {
            return Err(unavailable());
        }
        if value["value"].is_null() {
            return Err(failure("AGENT_NOT_FOUND", "Проект не найден."));
        }
    }
    if action == "delete" && args.get("area") != Some(&json!("files")) {
        return Err(failure(
            "AGENT_INVALID",
            "Удалять можно только рабочие файлы. Память изменяйте через save или edit.",
        ));
    }
    let write = matches!(
        action,
        "save" | "edit" | "delete" | "restore" | "policy_save"
    );
    if !matches!(
        action,
        "read"
            | "bundle"
            | "list"
            | "history"
            | "version"
            | "save"
            | "edit"
            | "restore"
            | "policy"
            | "policy_save"
            | "projection"
            | "delete"
            | "export"
    ) {
        return Err(failure("AGENT_INVALID", "Неизвестное действие."));
    }
    let base = paths::base(root, write)?;
    // Recovery runs under the same lease, including a read after an interrupted commit.
    // Readers share the writer lease, so a context bundle cannot mix revisions.
    let _lease = if base.is_some() {
        Some(transaction::lease(
            base.as_deref().ok_or_else(unavailable)?,
        )?)
    } else {
        None
    };
    if let Some(base) = &base {
        if base.join("pending.json").exists() {
            transaction::recover(base)?;
        }
    }
    match action {
        "read" => snapshot(base.as_deref(), args),
        "export" => {
            if args.get("area") != Some(&json!("files")) {
                return Err(failure(
                    "AGENT_INVALID",
                    "Скачивание доступно для рабочих файлов.",
                ));
            }
            let relative = paths::relative(args)?;
            let bytes = base
                .as_deref()
                .map(|base| {
                    paths::target(base, &relative, false).and_then(|p| read(&p, FILE_LIMIT))
                })
                .transpose()?
                .flatten()
                .ok_or_else(|| {
                    failure(
                        "AGENT_NOT_FOUND",
                        "Файл находится в истории. Сначала восстановите его.",
                    )
                })?;
            if text(args, "expected_revision")? != digest(&bytes) {
                return Err(failure("AGENT_CONFLICT","Файл обновился в другом окне. Откройте его актуальную версию перед скачиванием."));
            }
            let raw = std::str::from_utf8(&bytes)
                .map_err(|_| failure("AGENT_ENCODING", "Сохраните рабочий файл в UTF-8."))?;
            Ok(
                json!({"filename":relative.rsplit('/').next().unwrap(),"text":raw,"revision":digest(&bytes)}),
            )
        }
        "policy" => Ok(
            json!({"value":policy(base.as_deref())?,"revision":match &base{Some(base)=>revision(read(&paths::target(base,"policy.json",false)?,4096)?.as_deref()),None=>"missing".into()}}),
        ),
        "bundle" | "projection" => {
            let mut documents = Vec::new();
            for (area, project) in [("user", 0), ("memory", 0), ("memory", id)] {
                if id == 0 && documents.len() == 2 {
                    break;
                }
                documents.push(snapshot(
                    base.as_deref(),
                    json!({"area":area,"project_id":project})
                        .as_object()
                        .unwrap(),
                )?);
            }
            if action == "projection" {
                return Ok(projection::records(&documents, id));
            }
            Ok(json!({"documents":documents,"policy":policy(base.as_deref())?,"project_id":id}))
        }
        "list" => transaction::list(
            base.as_deref(),
            id,
            args.get("include_deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        "history" => transaction::history(
            base.as_deref(),
            &paths::relative(args)?,
            args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize,
        ),
        "version" => transaction::version(
            base.as_deref(),
            &paths::relative(args)?,
            text(args, "history_id")?,
        ),
        _ => {
            let base = base.as_deref().ok_or_else(unavailable)?;
            if args.get("actor").and_then(Value::as_str) == Some("agent") {
                let key = if args["area"] == "files" {
                    "workspace_write"
                } else {
                    "auto_memory"
                };
                if action == "policy_save"
                    || args["area"] == "policy"
                    || policy(Some(base))?[key] != true
                {
                    return Err(failure(
                        "AGENT_DISABLED",
                        "Автоматическое изменение отключено в настройках памяти.",
                    ));
                }
            }
            transaction::change(base, action, args)
        }
    }
}
