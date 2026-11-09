//! Local, authenticated mail routes owned by the native candidate.
mod imap;
mod outlook;
use crate::{auth, json_response, transport, Context};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::Response,
};
use les_core::catalog::mail;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

type Result<T> = std::result::Result<T, &'static str>;
const BAD: &str = "MAIL_REQUEST_INVALID";
const ENGINE: &str = "rust.mail";
#[derive(Clone, Copy)]
enum Route<'a> {
    Accounts,
    Status,
    Create,
    Update(&'a str),
    Test(&'a str),
    Sync(&'a str),
    Register,
    Import,
    Run,
    List,
    Message(&'a str),
    Open(&'a str),
    Thread(&'a str),
}
fn route<'a>(method: &str, path: &'a str) -> Option<Route<'a>> {
    use Route::*;
    let tail = path.strip_prefix("/api/mail/")?;
    let mut parts = tail.split('/');
    let root = parts.next()?;
    let id = parts.next();
    let action = parts.next();
    if parts.next().is_some() {
        return None;
    }
    Some(match (method, root, id, action) {
        ("GET", "accounts", None, None) => Accounts,
        ("GET", "status", None, None) => Status,
        ("POST", "accounts", None, None) => Create,
        ("PATCH", "accounts", Some(id), None) => Update(id),
        ("POST", "accounts", Some(id), Some("test")) => Test(id),
        ("POST", "accounts", Some(id), Some("sync")) => Sync(id),
        ("POST", "collector", Some("register-store"), None) => Register,
        ("POST", "collector", Some("import"), None) => Import,
        ("POST", "collector", Some("run"), None) => Run,
        ("GET", "messages", None, None) => List,
        ("GET", "messages", Some(id), None) => Message(id),
        ("POST", "messages", Some(id), Some("open")) => Open(id),
        ("GET", "threads", Some(id), None) => Thread(id),
        _ => return None,
    })
}
pub use imap::recover;

fn poll_seconds(state: &std::path::Path) -> u64 {
    if std::env::var("LES_STARTUP_BACKGROUND_MUTATIONS").as_deref() != Ok("true") {
        return 0;
    }
    let Ok(settings) = crate::index_batch::configuration::Settings::load(state) else {
        return 0;
    };
    settings
        .get("MAIL_IMAP_POLL_SEC", "180")
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .map(|seconds| seconds.clamp(60, 86_400))
        .unwrap_or(0)
}

pub async fn schedule(context: Arc<Context>) {
    if std::env::var("LES_STARTUP_BACKGROUND_MUTATIONS").as_deref() != Ok("true") {
        return;
    }
    tokio::time::sleep(Duration::from_secs(25)).await;
    loop {
        let worker = context.clone();
        let delay = tokio::task::spawn_blocking(move || {
            let seconds = poll_seconds(&worker.state_root);
            if seconds > 0 {
                if let Err(code) = imap::poll(&worker) {
                    if code != "MAIL_SYNC_BUSY" {
                        eprintln!("{}", json!({"event":"native_mail_poll_failed","code":code}));
                    }
                }
            }
            if seconds == 0 {
                300
            } else {
                seconds
            }
        })
        .await
        .unwrap_or(300);
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
}

pub fn handles(method: &str, path: &str) -> bool {
    route(method, path).is_some()
}
fn failure(code: &str) -> Response {
    let (status, message) = match code {
        BAD => (
            StatusCode::BAD_REQUEST,
            "Проверьте данные почтового аккаунта или формат запроса.",
        ),
        "MAIL_ORIGIN_DENIED" => (StatusCode::FORBIDDEN, "Почта доступна из локального Леса."),
        "MAIL_ACCOUNT_NOT_FOUND" | "MAIL_MESSAGE_NOT_FOUND" => {
            (StatusCode::NOT_FOUND, "Аккаунт или письмо не найдено.")
        }
        "MAIL_SYNC_BUSY" => (
            StatusCode::CONFLICT,
            "Синхронизация уже запущена. Дождитесь её окончания.",
        ),
        "MAIL_SECRET_MISSING" | "MAIL_SECRET_RECONFIRM_REQUIRED" => (
            StatusCode::CONFLICT,
            "Введите пароль приложения для этого почтового аккаунта.",
        ),
        "MAIL_ACCOUNT_DISABLED" => (
            StatusCode::CONFLICT,
            "Включите почтовый аккаунт перед синхронизацией.",
        ),
        "MAIL_IMAP_LOGIN_FAILED" => (
            StatusCode::BAD_GATEWAY,
            "Почта отклонила вход. Проверьте имя пользователя и пароль приложения.",
        ),
        "MAIL_IMAP_TLS_REQUIRED" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Для внешней почты требуется защищённое подключение IMAP.",
        ),
        "MAIL_IMAP_TLS_FAILED" => (
            StatusCode::BAD_GATEWAY,
            "Не удалось установить защищённое соединение с почтой. Проверьте имя сервера, порт и сертификат.",
        ),
        "MAIL_IMAP_CONNECT_FAILED" => (
            StatusCode::BAD_GATEWAY,
            "Сервер почты недоступен. Проверьте адрес, порт и подключение.",
        ),
        "MAIL_FORMAT_UNSUPPORTED" => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Этот формат почтового файла пока недоступен в новом Лесе.",
        ),
        "MAIL_ORIGINAL_CHANGED" | "MAIL_ORIGINAL_UNAVAILABLE" => (
            StatusCode::CONFLICT,
            "Сохранённый оригинал письма изменён или недоступен.",
        ),
        "MAIL_OUTLOOK_COLLECTOR_UNAVAILABLE" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Установите сборщик для классического Outlook.",
        ),
        "MAIL_OUTLOOK_MESSAGE_REQUIRED" => {
            (StatusCode::CONFLICT, "Это письмо нельзя открыть в Outlook.")
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Почтовая операция не завершилась. Проверьте состояние аккаунта и повторите.",
        ),
    };
    json_response(status, json!({"code":code,"detail":message}), ENGINE)
}
fn fields(raw: &str) -> Result<Value> {
    let mut result = serde_json::Map::new();
    let decode = |s: &str| {
        percent_encoding::percent_decode_str(&s.replace('+', " "))
            .decode_utf8()
            .map(|v| v.into_owned())
            .map_err(|_| BAD)
    };
    if raw.len() > 32768 {
        return Err(BAD);
    }
    for piece in raw.split('&').filter(|v| !v.is_empty()) {
        let (key, value) = piece.split_once('=').ok_or(BAD)?;
        if result.insert(decode(key)?, json!(decode(value)?)).is_some() {
            return Err(BAD);
        }
    }
    Ok(Value::Object(result))
}
async fn parse_json(request: Request) -> Result<Value> {
    let bytes = to_bytes(request.into_body(), 32768)
        .await
        .map_err(|_| BAD)?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or(BAD)
}
async fn multipart(request: Request) -> Result<(String, Vec<u8>, Value)> {
    const NAMES: [&str; 8] = [
        "message",
        "store_id",
        "entry_id",
        "store_label",
        "folder_id",
        "folder_path",
        "internet_message_id",
        "received_at",
    ];
    let boundary = multer::parse_boundary(
        request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .ok_or(BAD)?,
    )
    .map_err(|_| BAD)?;
    let constraints = multer::Constraints::new()
        .allowed_fields(NAMES.to_vec())
        .size_limit(
            multer::SizeLimit::new()
                .whole_stream(mail::MAX_MESSAGE as u64 + 32768)
                .per_field(8192)
                .for_field("message", mail::MAX_MESSAGE as u64),
        );
    let mut reader = multer::Multipart::with_constraints(
        request.into_body().into_data_stream(),
        boundary,
        constraints,
    );
    let mut data = serde_json::Map::new();
    let mut message = None;
    while let Some(field) = reader.next_field().await.map_err(|_| BAD)? {
        let name = field.name().ok_or(BAD)?.to_owned();
        if name == "message" {
            if message.is_some() {
                return Err(BAD);
            }
            let suffix = field
                .file_name()
                .and_then(|v| v.rsplit_once('.'))
                .map(|(_, v)| v.to_ascii_lowercase())
                .ok_or(BAD)?;
            if !matches!(suffix.as_str(), "eml" | "msg") {
                return Err("MAIL_FORMAT_UNSUPPORTED");
            }
            message = Some((suffix, field.bytes().await.map_err(|_| BAD)?.to_vec()));
        } else {
            let bytes = field.bytes().await.map_err(|_| BAD)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
            if data.insert(name, json!(text)).is_some() {
                return Err(BAD);
            }
        }
    }
    let (suffix, bytes) = message.ok_or(BAD)?;
    Ok((suffix, bytes, Value::Object(data)))
}
fn register(context: &Context, fields: &Value) -> Result<Value> {
    mail::create(
        &context.state_root,
        &context.database,
        &json!({"kind":"outlook_classic",
        "label":fields["store_label"].as_str().filter(|s|!s.trim().is_empty()).unwrap_or("Outlook"),
        "native_account_id":fields["store_id"]}),
        true,
    )
}
fn import(context: &Context, suffix: &str, bytes: &[u8], f: &Value) -> Result<Value> {
    if f["entry_id"].as_str().is_none_or(str::is_empty) {
        return Err(BAD);
    }
    let a = register(context, f)?;
    let id = a["id"].as_str().ok_or(BAD)?;
    let result = mail::register(
        &context.state_root,
        id,
        suffix,
        bytes,
        &json!({"native_id":f["entry_id"],"outlook_entry_id":f["entry_id"],
        "outlook_store_id":f["store_id"],"folder_id":f["folder_id"],"folder_path":f["folder_path"],
        "internet_message_id":f["internet_message_id"],"received_at":f["received_at"]}),
    )?;
    crate::intake::wake(context);
    Ok(
        json!({"status":"accepted","created":result["duplicate"]==false,"account_id":id,"dataset_id":a["dataset_id"],
        "snapshot_id":result["content_sha256"],"message_id":result["id"],"index_status":"queued"}),
    )
}
fn authorize_collector(headers: &HeaderMap) -> bool {
    !headers.contains_key("origin")
        && !headers.contains_key("sec-fetch-site")
        && !headers.contains_key("x-forwarded-for")
        && !headers.contains_key("forwarded")
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    use Route::*;
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let Some(route) = route(&method, &path) else {
        return failure(BAD);
    };
    let collector = matches!(route, Register | Import);
    if !transport::local_browser(request.headers())
        || collector && !authorize_collector(request.headers())
    {
        return failure("MAIL_ORIGIN_DENIED");
    }
    let cors = if collector {
        HeaderMap::new()
    } else {
        let cors = match auth::authorize(
            &context,
            request.headers(),
            method != "GET" && !matches!(route, Open(_)),
        )
        .await
        {
            Ok(v) => v,
            Err(response) => return response,
        };
        if !auth::origin_allowed(request.headers(), &cors) {
            return failure("MAIL_ORIGIN_DENIED");
        }
        cors
    };
    let query = fields(request.uri().query().unwrap_or(""));
    let mut response = match tokio::time::timeout(
        Duration::from_secs(30),
        receive(context, request, route, query),
    )
    .await
    {
        Ok(Ok((status, value))) => json_response(status, value, ENGINE),
        Ok(Err(code)) => failure(code),
        Err(_) => failure("MAIL_TIMEOUT"),
    };
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
async fn receive(
    context: Arc<Context>,
    request: Request,
    route: Route<'_>,
    query: Result<Value>,
) -> Result<(StatusCode, Value)> {
    use Route::*;
    let query = query?;
    let input = match route {
        Create | Update(_) | Sync(_) => parse_json(request).await?,
        Register => {
            let bytes = to_bytes(request.into_body(), 32768)
                .await
                .map_err(|_| BAD)?;
            fields(std::str::from_utf8(&bytes).map_err(|_| BAD)?)?
        }
        Import => {
            let _slot = context
                .upload_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| "MAIL_SYNC_BUSY")?;
            let (suffix, bytes, fields) = multipart(request).await?;
            let value = tokio::task::spawn_blocking(move || {
                let _slot = _slot;
                import(&context, &suffix, &bytes, &fields)
            })
            .await
            .map_err(|_| "MAIL_REGISTRY_UNAVAILABLE")??;
            return Ok((StatusCode::ACCEPTED, value));
        }
        _ => Value::Null,
    };
    let code = if matches!(route, Sync(_)) {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    let (op, id) = match route {
        Accounts => ("accounts", ""),
        Status => ("status", ""),
        Create => ("create", ""),
        Update(id) => ("update", id),
        Test(id) => ("test", id),
        Sync(id) => ("sync", id),
        Register => ("register", ""),
        Run => ("run", ""),
        List => ("list", ""),
        Message(id) => ("message", id),
        Open(id) => ("open", id),
        Thread(id) => ("thread", id),
        Import => return Err(BAD),
    };
    let id = id.to_owned();
    let value=tokio::task::spawn_blocking(move||{
        let state=&context.state_root;
        let database=&context.database;
        match op {
            "accounts"=>mail::accounts(state),
            "status"=>{let counts=mail::accounts(state)?;Ok(json!({"component":"Е.Ж.И.К.","accounts":counts["accounts"],"folders":counts["folders"],"owner":"rust.mail"}))},
            "create"=>{if input["kind"]!="imap"{return Err(BAD);}Ok(json!({"status":"created","account":mail::create(state,database,&input,false)?}))},
            "update"=>Ok(json!({"status":"updated","account":imap::update_account(state,&id,&input)?})),
            "test"=>imap::test(state,&id),
            "sync"=>imap::start(context.clone(),id,input),
            "register"=>{let a=register(&context,&input)?;Ok(json!({"status":"ready","account_id":a["id"],"dataset_id":a["dataset_id"],"dataset_name":a["dataset_name"]}))},
            "run"=>{outlook::launch(&context,None)?;Ok(json!({"status":"started","mode":"manual","hard_limit_seconds":15}))},
            "list"=>mail::list(state,query["account_id"].as_str(),None,query["limit"].as_str().and_then(|v|v.parse().ok()).unwrap_or(50)),
            "message"=>mail::read(state,database,&id),
            "thread"=>mail::list(state,None,Some(&id),100),
            "open"=>{
                let m=mail::message(state,database,&id)?;
                if m["source_kind"]!="outlook_classic"{return Err("MAIL_OUTLOOK_MESSAGE_REQUIRED");}
                outlook::launch(&context,Some((m["outlook_store_id"].as_str().ok_or(BAD)?,m["outlook_entry_id"].as_str().ok_or(BAD)?)))?;
                Ok(json!({"status":"opening","message_id":id}))
            }
            _=>Err(BAD)
        }
    }).await.map_err(|_|"MAIL_REGISTRY_UNAVAILABLE")??;
    Ok((code, value))
}
