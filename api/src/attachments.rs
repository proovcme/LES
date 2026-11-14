//! Opaque originals and bounded parsing in a supervised child.
mod folder;
mod upload;
use crate::{index_batch::configuration::Settings, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub(crate) use upload::scratch;
pub(crate) use upload::Temporary;
pub use upload::{handle_public, public_path};
pub const PRIVATE: &str = "/api/light/internal-attachments";
pub fn handles(path: &str) -> bool {
    path == PRIVATE || path == upload::PATH
}
type Result<T> = std::result::Result<T, String>;
pub struct Policy {
    pub root: PathBuf,
    pub age: u64,
    pub max_chars: u64,
}
pub async fn policy(context: &Context) -> Result<Policy> {
    let state = context.state_root.clone();
    tokio::task::spawn_blocking(move || {
        let settings = Settings::load(&state)?;
        let raw = settings.get("LES_CHAT_ATTACHMENT_ROOT", "");
        let root = if raw.is_empty() {
            state.join("storage/chat_attachments")
        } else {
            let p = PathBuf::from(raw);
            if p.is_absolute() {
                p
            } else {
                state.join(p)
            }
        };
        let age = settings
            .get("LES_CHAT_ATTACHMENT_MAX_AGE_SEC", "604800")
            .parse::<u64>()
            .map_err(|_| "ATTACHMENT_POLICY_INVALID")?;
        let max_chars = settings
            .get("RAG_ATTACH_READ_MAX_CHARS", "18000")
            .parse::<u64>()
            .map_err(|_| "ATTACHMENT_POLICY_INVALID")?
            .min(20000);
        Ok::<_, String>(Policy {
            root,
            age,
            max_chars,
        })
    })
    .await
    .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE".to_owned())?
}
pub async fn worker(input: Value) -> Result<Value> {
    tokio::task::spawn_blocking(move||{
        let program=std::env::current_exe().map_err(|_|"ATTACHMENT_WORKER_UNAVAILABLE")?;
        let value=les_core::run_native_attachments(&json!({"schema":"les.worker-host.v1","program":program,"directory":program.parent(),"args":["--serve-chat-attachments"],"context":"les.chat-attachments.v1","timeout_ms":120000,"input":input}))?;
        let result=&value["result"];if value["status"]!="completed"||result["status"]!="completed"{
            return Err(result["code"].as_str().or_else(||value["code"].as_str()).unwrap_or("ATTACHMENT_READ_FAILED").to_owned());}
        Ok(result["value"].clone())
    }).await.map_err(|_|"ATTACHMENT_WORKER_UNAVAILABLE".to_owned())?
}
pub async fn prepare(
    context: Arc<Context>,
    path: PathBuf,
    name: String,
    ident: Option<String>,
) -> Result<Value> {
    prepare_asset(context, path, name, ident, None).await
}
async fn prepare_asset(
    context: Arc<Context>,
    path: PathBuf,
    name: String,
    ident: Option<String>,
    claim: Option<les_core::chat_attachments::UploadClaim>,
) -> Result<Value> {
    if !path.is_absolute() {
        return Err("ATTACHMENT_PATH_UNSAFE".into());
    }
    let _slot = tokio::time::timeout(
        Duration::from_secs(30),
        context.attachment_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?
    .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?;
    let settings = policy(&context).await?;
    let root = settings.root.clone();
    let age = settings.age;
    let (claim, replay) = tokio::task::spawn_blocking(move || {
        let replay = claim
            .as_ref()
            .map(|claim| les_core::chat_attachments::replay_upload(&root, claim, age))
            .transpose()?
            .flatten();
        Ok::<_, &'static str>((claim, replay))
    })
    .await
    .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")??;
    if let Some(replay) = replay {
        return Ok(replay);
    }
    let mut input = json!({"action":"prepare","path":path,"name":name,"max_chars":settings.max_chars,"owned_state":context.state_root});
    if let Some(claim) = &claim {
        input["source_sha256"] = json!(claim.digest());
    }
    let preview = worker(input).await?;
    let asset = ident
        .map(Ok)
        .unwrap_or_else(les_core::chat_attachments::new_id)
        .map_err(str::to_owned)?;
    let root = settings.root.clone();
    let id = asset.clone();
    let age = settings.age;
    let original = name.clone();
    let result = json!({"attachment_id":asset,"mode":"read","name":name,"chars":preview["chars"].as_u64().unwrap_or(0),"text":preview["text"].as_str().unwrap_or(""),"truncated":preview["truncated"].as_bool().unwrap_or(false),"media_type":preview["media_type"].as_str().unwrap_or("document")});
    if let Some(claim) = claim {
        return tokio::task::spawn_blocking(move || {
            les_core::chat_attachments::cleanup(&root, age)?;
            les_core::chat_attachments::save_upload(&root, &path, &original, &claim, &result)
        })
        .await
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?
        .map_err(str::to_owned);
    }
    let metadata = tokio::task::spawn_blocking(move || {
        les_core::chat_attachments::cleanup(&root, age)?;
        les_core::chat_attachments::preserve(&root, &path, &id, &original)
    })
    .await
    .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?
    .map_err(str::to_owned)?;
    if preview["source_sha256"].is_string() && preview["source_sha256"] != metadata["sha256"] {
        let id = asset.clone();
        let _ = tokio::task::spawn_blocking(move || {
            les_core::chat_attachments::consume(&settings.root, &id)
        })
        .await;
        return Err("ATTACHMENT_HASH_CHANGED".into());
    }
    Ok(result)
}
pub async fn bound(context: Arc<Context>, ident: String, owner: Option<String>) -> Result<Value> {
    let policy = policy(&context).await?;
    let root = policy.root.clone();
    let id = ident.clone();
    let age = policy.age;
    let gate = owner.clone();
    let asset = tokio::task::spawn_blocking(move || {
        les_core::chat_attachments::resolve_for(&root, &id, age, gate.as_deref())
    })
    .await
    .map_err(|_| "ATTACHMENT_READ_FAILED")?
    .map_err(str::to_owned)?;
    let name = asset.metadata["original_name"].clone();
    let hash = asset.metadata["sha256"].clone();
    let image = asset.metadata["suffix"]
        .as_str()
        .is_some_and(|s| matches!(s, ".png" | ".jpg" | ".jpeg" | ".webp"));
    let folder = asset.metadata["upload_response"]["kind"] == "folder";
    let _slot = tokio::time::timeout(
        Duration::from_secs(30),
        context.attachment_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?
    .map_err(|_| "ATTACHMENT_QUEUE_TIMEOUT")?;
    let input = if image {
        json!({"schema":"les.chat-attachments.v1","action":"resolve_image","root":policy.root,"attachment_id":ident,"max_age_sec":age,"upload_owner":owner})
    } else {
        json!({"action":"prepare","path":asset.path,"name":name,"source_sha256":hash,
            "max_chars":if folder{48000}else{policy.max_chars},"full_snapshot":folder,"owned_state":context.state_root})
    };
    drop(asset);
    let mut result = worker(input).await?;
    if result["source_sha256"].is_string() && result["source_sha256"] != hash {
        return Err("ATTACHMENT_HASH_CHANGED".into());
    }
    result["name"] = name;
    result["source_sha256"] = hash;
    result["attachment_id"] = json!(ident);
    result["media_type"] = json!(if image { "image" } else { "document" });
    Ok(result)
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    if request.method() != "POST"
        || context
            .local_model
            .as_ref()
            .is_none_or(|b| !b.authorized(request.headers()))
    {
        return crate::error(StatusCode::NOT_FOUND, "Not Found");
    }
    if request.uri().path() == upload::PATH {
        return upload::handle(context, request).await;
    }
    let bytes = match to_bytes(request.into_body(), 16384).await {
        Ok(v) => v,
        Err(_) => return failure("ATTACHMENT_REQUEST_INVALID"),
    };
    let input: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return failure("ATTACHMENT_REQUEST_INVALID"),
    };
    let path = match input["path"].as_str() {
        Some(s) => Path::new(s).to_owned(),
        None => return failure("ATTACHMENT_REQUEST_INVALID"),
    };
    let name = match input["name"].as_str() {
        Some(s) => s.to_owned(),
        None => return failure("ATTACHMENT_REQUEST_INVALID"),
    };
    match prepare(
        context,
        path,
        name,
        input["attachment_id"].as_str().map(str::to_owned),
    )
    .await
    {
        Ok(value) => crate::json_response(StatusCode::OK, value, "rust.attachments"),
        Err(code) => failure(&code),
    }
}
pub fn failure(code: &str) -> Response {
    let status = if code == "ATTACHMENT_NOT_FOUND" || code == "ATTACHMENT_EXPIRED" {
        StatusCode::NOT_FOUND
    } else if matches!(
        code,
        "ATTACHMENT_KEY_REUSED"
            | "ATTACHMENT_KEY_INVALID"
            | "ATTACHMENT_ID_CONFLICT"
            | "ATTACHMENT_INDEX_DATASET_CONFLICT"
            | "ATTACHMENT_INDEX_BUSY"
    ) {
        StatusCode::CONFLICT
    } else if code == "INTAKE_OUTSIDE_ALLOWED_ROOT" {
        StatusCode::FORBIDDEN
    } else if code == "ATTACHMENT_QUEUE_BUSY" {
        StatusCode::TOO_MANY_REQUESTS
    } else if code.contains("UNAVAILABLE") || code.contains("TIMEOUT") {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let detail = detail(code);
    eprintln!("{}", json!({"event":"chat_attachment_failed","code":code}));
    crate::json_response(
        status,
        json!({"code":code,"detail":detail}),
        "rust.attachments",
    )
}
pub(crate) fn detail(code: &str) -> &'static str {
    match code{
        "ATTACHMENT_NOT_FOUND"=>"Вложение больше недоступно. Прикрепите файл заново.",
        "ATTACHMENT_EXPIRED"=>"Срок хранения вложения истёк. Прикрепите файл заново.",
        "ATTACHMENT_UPLOAD_LIMIT"=>"Файл превышает разрешённый размер загрузки. Выберите меньший файл или добавьте исходную папку в набор документов.",
        "ATTACHMENT_EMPTY"=>"Файл пустой. Выберите файл с содержимым.",
        "ATTACHMENT_NAME_INVALID"=>"Недопустимое имя файла. Используйте обычное имя без пути и служебных символов.",
        "ATTACHMENT_IMAGE_PIXELS"=>"Изображение больше 25 мегапикселей. Выберите меньший файл.",
        "ATTACHMENT_IMAGE_LIMIT"=>"Изображение больше 10 МиБ. Выберите меньший файл.",
        "ATTACHMENT_IMAGE_FORMAT"|"ATTACHMENT_IMAGE_INVALID"=>"Изображение повреждено или имеет неподдерживаемый формат. Выберите PNG, JPEG или WebP.",
        "ATTACHMENT_HASH_CHANGED"|"ATTACHMENT_SIZE_CHANGED"=>"Вложение изменилось. Прикрепите файл заново.",
        "ATTACHMENT_KEY_REUSED"|"ATTACHMENT_ID_CONFLICT"=>"С этим ключом уже загружено другое вложение. Отправьте новый запрос с новым ключом.",
        "ATTACHMENT_KEY_INVALID"=>"Ключ повторной загрузки некорректен. Отправьте один ключ до 256 байт.",
        "ATTACHMENT_QUEUE_BUSY"=>"Лес уже принимает файлы. Дождитесь завершения текущей загрузки и повторите.",
        "ATTACHMENT_INDEX_UNAVAILABLE"=>"Очередь индексации не запущена. Пока можно прикрепить файл для чтения.",
        "ATTACHMENT_INDEX_DATASET_CONFLICT"=>"Набор «Вложения чата» неоднозначен или разрешён для внешних моделей. Выберите локальный набор или переименуйте дубликат.",
        "ATTACHMENT_INDEX_BUSY"=>"Набор «Вложения чата» сейчас импортирует папку. Дождитесь завершения и повторите.",
        "ATTACHMENT_FOLDER_LIMIT"=>"Папка превышает лимит одного вложения. Добавьте её как набор документов.",
        "ATTACHMENT_FOLDER_EMPTY"=>"В папке нет поддерживаемых документов.",
        "ATTACHMENT_FOLDER_INCOMPLETE"=>"Не удалось прочитать весь комплект. Вложение не создано; проверьте файлы или добавьте папку как набор.",
        "INTAKE_OUTSIDE_ALLOWED_ROOT"=>"Папка находится вне разрешённых источников Леса.",
        "ATTACHMENT_NO_TEXT"=>"Текст не найден. Для скана включите OCR или добавьте файл в набор документов.",
        "WORKBOOK_COMPATIBILITY"=>"Для этой книги ещё нужен совместимый обработчик. Нативное чтение остановлено; проверка таблицы не выполнена.",
        _=>"Не удалось прочитать вложение. Проверьте формат файла и журнал Леса; действие не завершено."
    }
}
