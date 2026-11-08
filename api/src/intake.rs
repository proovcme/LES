//! Local folder registration runs outside request handling and checkpoints each batch.
use crate::{auth, error, json_response, library::catalog, transport, Context};
use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::StatusCode,
    response::Response,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, Metadata},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use tokio::sync::Notify;

pub const PATH: &str = "/api/rag/index-external";
const POLICY: &str = "/api/light/internal-intake-policy";
const START_INDEXING: &str = "/api/light/internal-intake-parse";
const TOKEN: &str = "x-les-intake-token";
const ENGINE: &str = "rust.intake";
const MAX_FILES: usize = 50_000;

pub struct Service {
    token: String,
    wake: Notify,
}

impl Service {
    pub fn new(token: String) -> Self {
        Self {
            token,
            wake: Notify::new(),
        }
    }
}

pub fn internal(path: &str) -> bool {
    matches!(path, POLICY | START_INDEXING)
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(bytes) = to_bytes(body, 64 * 1024).await else {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Запрос на подключение слишком большой.",
        );
    };
    let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Ожидается объект JSON.");
    };
    // The legacy synchronous contract still returns the full preview and parse job.
    if body["background"] != true {
        return transport::forward(context, Request::from_parts(parts, Body::from(bytes))).await;
    }
    let cors = match auth::authorize(&context, &parts.headers, true).await {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let mut response = enqueue(&context, &body).await;
    response.headers_mut().extend(cors);
    response
}

async fn enqueue(context: &Context, body: &Value) -> Response {
    let Some(path) = body["path"]
        .as_str()
        .map(str::trim)
        .filter(|path| !path.is_empty() && !path.contains('\0') && path.len() <= 32 * 1024)
    else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "Укажите путь к папке.");
    };
    let Some(dataset) = body["dataset_id"].as_str().filter(|id| {
        !matches!(*id, "" | "." | "..")
            && id.len() <= 160
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    }) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Укажите ID набора документов.",
        );
    };
    let parse = body.get("parse").map_or(Some(true), Value::as_bool);
    let limit = body.get("parse_limit").map_or(Some(25), Value::as_i64);
    let (Some(parse), Some(limit @ 1..=500)) = (parse, limit) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Проверьте параметры индексации.",
        );
    };
    if body
        .get("auto_split")
        .is_some_and(|value| !value.is_boolean())
        || body.get("split_max_mb").is_some_and(|value| {
            !value
                .as_f64()
                .is_some_and(|size| (5.0..=500.0).contains(&size))
        })
    {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Проверьте параметры обработки PDF.",
        );
    }
    let args = json!({"root":path,"dataset_id":dataset,"parse_limit":if parse {limit} else {0}});
    let db = context.database.clone();
    let result = tokio::task::spawn_blocking(move || catalog(&db, "intake_enqueue", args)).await;
    let Ok(result) = result else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось поставить папку в очередь.",
        );
    };
    if result["status"] == "completed" && result["value"]["id"].is_string() {
        context.intake.as_ref().unwrap().wake.notify_one();
        return json_response(
            StatusCode::OK,
            json!({
                "status":"started", "dataset_id":dataset, "dataset_name":result["value"]["dataset_name"],
                "intake_job_id":result["value"]["id"], "note":"Лес проверит папку в фоне."
            }),
            ENGINE,
        );
    }
    match result["code"].as_str() {
        Some("WORKSPACE_NOT_FOUND") => error(StatusCode::NOT_FOUND, "Набор документов не найден."),
        Some("WORKSPACE_CONFLICT") => error(
            StatusCode::CONFLICT,
            result["detail"].as_str().unwrap_or_default(),
        ),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось записать очередь импорта. Повторите подключение.",
        ),
    }
}

#[derive(Debug)]
struct Stop {
    code: &'static str,
    message: &'static str,
}

const STORAGE: Stop = Stop {
    code: "INTAKE_STORAGE_UNAVAILABLE",
    message:
        "Не удалось сохранить прогресс. Проверьте доступ к папке данных Леса и повторите импорт.",
};

fn source_error(error: std::io::Error) -> Stop {
    use std::io::ErrorKind::*;
    match error.kind() {
        NotFound => Stop {
            code: "INTAKE_SOURCE_MISSING",
            message: "Папка или файл исчезли во время импорта. Проверьте, что источник доступен.",
        },
        PermissionDenied => Stop {
            code: "INTAKE_ACCESS_DENIED",
            message: "Лес не может прочитать папку или файл. Проверьте права доступа.",
        },
        _ => Stop {
            code: "INTAKE_SOURCE_UNREADABLE",
            message:
                "Не удалось полностью прочитать папку. Проверьте диск или сетевое подключение.",
        },
    }
}

fn changed() -> Stop {
    Stop {
        code: "INTAKE_SOURCE_CHANGED",
        message:
            "Путь к источнику изменился во время импорта. Проверьте папку и подключите её снова.",
    }
}

struct Policy {
    roots: Vec<PathBuf>,
    allow_any: bool,
    suffixes: HashSet<String>,
}

impl Policy {
    fn allows(&self, canonical: &Path) -> bool {
        self.allow_any || self.roots.iter().any(|root| canonical.starts_with(root))
    }

    fn root(&self, raw: &str) -> Result<PathBuf, Stop> {
        let raw = Path::new(raw);
        let expanded;
        let path = if let Ok(suffix) = raw.strip_prefix("~") {
            let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
            expanded = PathBuf::from(home.ok_or_else(changed)?).join(suffix);
            &expanded
        } else {
            raw
        };
        let canonical = path.canonicalize().map_err(source_error)?;
        if !self.allows(&canonical) {
            return Err(Stop {
                code: "INTAKE_OUTSIDE_ALLOWED_ROOT",
                message: "Папка находится вне разрешённых источников Леса.",
            });
        }
        if !canonical.metadata().map_err(source_error)?.is_dir() {
            return Err(Stop {
                code: "INTAKE_NOT_DIRECTORY",
                message: "Для подключения набора выберите папку.",
            });
        }
        Ok(canonical)
    }
}

fn source_name(path: &Path) -> Result<&str, Stop> {
    path.to_str().ok_or(Stop {
        code: "INTAKE_PATH_ENCODING",
        message: "В папке есть имя файла, которое нельзя прочитать как Unicode.",
    })
}

fn source_path(path: &Path) -> Result<String, Stop> {
    let text = source_name(path)?;
    #[cfg(windows)]
    if text.encode_utf16().count() < 260 {
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(format!(r"\\{unc}"));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            return Ok(local.to_owned());
        }
    }
    Ok(text.to_owned())
}

fn is_reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

fn scan(root: &Path, policy: &Policy) -> Result<Vec<Value>, Stop> {
    let mut stack = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    while let Some(folder) = stack.pop() {
        // A renamed directory or a junction must never redirect the walk.
        if folder.canonicalize().map_err(source_error)? != folder {
            return Err(changed());
        }
        for entry in fs::read_dir(folder).map_err(source_error)? {
            let entry = entry.map_err(source_error)?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).map_err(source_error)?;
            let name = entry.file_name();
            if meta.is_dir() {
                if name != "_originals" && !is_reparse(&meta) {
                    stack.push(path);
                }
                continue;
            }
            let Some(name) = name.to_str() else {
                return Err(changed());
            };
            if ["~$", ".~lock.", "._"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
                || matches!(
                    name,
                    "LES.md"
                        | "ЛЕС.md"
                        | "les.md"
                        | "лес.md"
                        | "00_dataset_map.md"
                        | ".pdf_preprocess_state.json"
                        | "_les_dataset_profile.json"
                )
                || !path
                    .extension()
                    .and_then(|suffix| suffix.to_str())
                    .is_some_and(|ext| {
                        policy
                            .suffixes
                            .contains(&format!(".{}", ext.to_lowercase()))
                    })
            {
                continue;
            }
            let resolved = match path.canonicalize() {
                Err(error) if is_reparse(&meta) && error.kind() == std::io::ErrorKind::NotFound => {
                    continue
                }
                other => other.map_err(source_error)?,
            };
            if !resolved.starts_with(root) || !resolved.metadata().map_err(source_error)?.is_file()
            {
                continue;
            }
            let relative = resolved
                .strip_prefix(root.parent().unwrap_or(root))
                .map_err(|_| changed())?;
            let name = source_name(relative)?.replace('\\', "/");
            let source = source_path(&resolved)?;
            files.insert(name.clone(), json!({"file_name":name,"source_path":source}));
            if files.len() > MAX_FILES {
                return Err(Stop {
                    code: "INTAKE_TOO_MANY_FILES",
                    message:
                        "В папке больше 50 000 поддерживаемых файлов. Подключите её по частям.",
                });
            }
        }
    }
    if files.is_empty() {
        return Err(Stop {
            code: "INTAKE_EMPTY",
            message: "В папке нет поддерживаемых документов.",
        });
    }
    Ok(files.into_values().collect())
}

fn call(database: &Path, action: &str, args: Value) -> Result<Value, Stop> {
    let result = catalog(database, action, args);
    if result["status"] == "completed" {
        return Ok(result["value"].clone());
    }
    if result["code"] == "WORKSPACE_NOT_FOUND" {
        return Err(Stop {
            code: "INTAKE_DATASET_REMOVED",
            message: "Набор документов удалён. Импорт остановлен.",
        });
    }
    eprintln!(
        "{}",
        json!({"event":"intake_sqlite","action":action,"code":result["code"],"sqlite_extended":result["sqlite_extended"]})
    );
    Err(STORAGE)
}

fn register(database: &Path, state: &Path, job: &Value, policy: &Policy) -> Result<(), Stop> {
    let root = policy.root(job["root"].as_str().unwrap_or_default())?;
    let id = &job["id"];
    if job["phase"] == "scan" {
        let files = scan(&root, policy)?;
        call(
            database,
            "intake_manifest",
            json!({"id":id,"root":source_name(&root)?,"files":files}),
        )?;
    } else if job["resolved_root"] != source_name(&root)? {
        return Err(changed());
    }
    if job["phase"] == "index" {
        return Ok(());
    }
    let dataset = job["dataset_id"].as_str().ok_or(STORAGE)?;
    fs::create_dir_all(state.join("storage/datasets").join(dataset)).map_err(|_| STORAGE)?;
    loop {
        let batch = call(database, "intake_batch", json!({"id":id}))?;
        let mut files = Vec::new();
        for item in batch
            .as_array()
            .filter(|items| !items.is_empty())
            .ok_or(STORAGE)?
        {
            let source = item["source_path"].as_str().ok_or(STORAGE)?;
            let path = Path::new(source).canonicalize().map_err(source_error)?;
            if !path.starts_with(&root) || source_path(&path)? != source {
                return Err(changed());
            }
            let stat = path.metadata().map_err(source_error)?;
            if !stat.is_file() {
                return Err(changed());
            }
            let modified = stat.modified().map_err(source_error)?;
            let mtime = match modified.duration_since(UNIX_EPOCH) {
                Ok(time) => time.as_secs_f64(),
                Err(error) => -error.duration().as_secs_f64(),
            };
            files.push(json!({"ordinal":item["ordinal"],"source_path":source,
                "file_size":stat.len(),"file_mtime":mtime}));
        }
        let progress = call(database, "intake_register", json!({"id":id,"files":files}))?;
        if progress["phase"] != "register" {
            return Ok(());
        }
    }
}

async fn policy(context: &Context, service: &Service) -> Option<Policy> {
    let mut response = context
        .client
        .get(format!("{}{POLICY}", context.upstream))
        .header(TOKEN, &service.token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 128 * 1024 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).ok()?;
    let allow_any = body["allow_any"].as_bool()?;
    let roots = body["roots"]
        .as_array()?
        .iter()
        .map(|root| PathBuf::from(root.as_str().unwrap_or_default()))
        .filter(|root| root.is_absolute())
        .collect::<Vec<_>>();
    let roots = tokio::task::spawn_blocking(move || {
        roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect()
    })
    .await
    .ok()?;
    let suffixes = body["suffixes"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Some(Policy {
        allow_any,
        roots,
        suffixes,
    })
}

async fn record(context: &Context, id: &Value, action: &'static str, stop: Option<Stop>) {
    let (reason, message) = stop.map_or(("", ""), |stop| (stop.code, stop.message));
    let database = context.database.clone();
    let args = json!({"id":id,"reason":reason,"message":message});
    if let Ok(Ok(progress)) =
        tokio::task::spawn_blocking(move || call(&database, action, args)).await
    {
        eprintln!(
            "{}",
            json!({"event":action,"job_id":id,"reason":reason,"progress":progress})
        );
    }
}

async fn start_indexing(context: &Context, service: &Service, job: &Value) -> Result<(), Stop> {
    let response = context
        .client
        .post(format!("{}{START_INDEXING}", context.upstream))
        .header(TOKEN, &service.token)
        .header("content-type", "application/json")
        .body(json!({"dataset_id":job["dataset_id"],"intake_id":job["id"]}).to_string())
        .timeout(Duration::from_secs(600))
        .send()
        .await;
    match response.map(|response| response.status()) {
        Ok(StatusCode::OK) => Ok(()),
        Ok(StatusCode::PRECONDITION_FAILED) => Err(Stop {
            code: "INDEX_NOT_CONFIGURED",
            message: "Файлы зарегистрированы. Для индексации проверьте модель поиска в настройках.",
        }),
        Ok(StatusCode::TOO_MANY_REQUESTS) => Err(Stop {
            code: "INDEX_MEMORY_GUARD",
            message: "Файлы зарегистрированы. Для индексации освободите оперативную память.",
        }),
        Ok(StatusCode::CONFLICT | StatusCode::NO_CONTENT) => Err(Stop {
            code: "INDEX_NO_PROGRESS",
            message:
                "Партия индексации пока не продвинулась. Лес повторит попытку через 30 секунд.",
        }),
        _ => Err(Stop {
            code: "INDEX_SERVICE_UNAVAILABLE",
            message: "Служба индексации недоступна. Лес повторит попытку через 30 секунд.",
        }),
    }
}

fn lease(state: &Path) -> Option<File> {
    let path = state.join("data");
    fs::create_dir_all(&path).ok()?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join("native-intake.lock"))
        .ok()?;
    lock.try_lock().ok()?;
    Some(lock)
}

pub async fn run(context: Arc<Context>) {
    let service = context.intake.as_ref().unwrap();
    let state = context.state_root.clone();
    let Ok(Some(_lease)) = tokio::task::spawn_blocking(move || lease(&state)).await else {
        eprintln!(
            "{}",
            json!({"event":"intake_worker_unavailable","reason":"INTAKE_WORKER_LEASE"})
        );
        return;
    };
    loop {
        let database = context.database.clone();
        let claimed = tokio::task::spawn_blocking(move || {
            if call(&database, "intake_has_work", json!({}))? == true {
                call(&database, "intake_claim", json!({}))
            } else {
                Ok(Value::Null)
            }
        })
        .await;
        if let Ok(Ok(job)) = claimed {
            if let Some(id) = job["id"].as_str() {
                if job["resumed"] == true {
                    eprintln!(
                        "{}",
                        json!({"event":"intake_resumed","job_id":id,"phase":job["phase"],
                        "processed":job["processed"],"reason":"INTAKE_INTERRUPTED"})
                    );
                }
                process(&context, service, job).await;
                continue;
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), service.wake.notified()).await;
    }
}

async fn process(context: &Context, service: &Service, job: Value) {
    let Some(policy) = policy(context, service).await else {
        record(
            context,
            &job["id"],
            "intake_wait",
            Some(Stop {
                code: "INTAKE_POLICY_UNAVAILABLE",
                message:
                    "Служба источников ещё запускается. Лес повторит проверку через несколько секунд.",
            }),
        )
        .await;
        return;
    };
    let database = context.database.clone();
    let state = context.state_root.clone();
    let input = job.clone();
    let outcome =
        tokio::task::spawn_blocking(move || register(&database, &state, &input, &policy)).await;
    let id = &job["id"];
    match outcome {
        Ok(Ok(())) if job["parse_limit"].as_i64().unwrap_or_default() > 0 => {
            let stop = start_indexing(context, service, &job).await.err();
            record(context, id, "intake_index_checkpoint", stop).await;
        }
        Ok(Ok(())) => eprintln!("{}", json!({"event":"intake_registered","job_id":id})),
        Ok(Err(stop)) => record(context, id, "intake_fail", Some(stop)).await,
        Err(_) => record(context, id, "intake_fail", Some(Stop {
            code:"INTAKE_WORKER_STOPPED", message:"Обработчик импорта остановился. Сохранённые документы доступны. Повторите подключение.",
        })).await,
    }
}
