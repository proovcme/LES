//! Streaming multipart intake. Parsing never runs in the HTTP task.
use super::*;
use multer::{Constraints, SizeLimit};
use tokio::io::AsyncWriteExt;
pub const PATH: &str = "/api/light/internal-attachments/upload";
const PUBLIC: &str = "/api/chat/attachments";
const LEGACY: &str = "/api/rag/attach";
const FOLDER: &str = "/api/rag/attach-folder";
pub fn public_path(path: &str) -> bool {
    matches!(path, PUBLIC | LEGACY | FOLDER)
}
pub(super) async fn scratch(context: &Context) -> Result<PathBuf> {
    let directory = context.state_root.join("storage/upload-tmp");
    let path = directory.clone();
    context
        .upload_owner
        .get_or_try_init(|| async move {
            tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
                std::fs::create_dir_all(&path).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
                let storage = path.parent().ok_or("ATTACHMENT_PATH_UNSAFE")?;
                if super::folder::reparse(
                    &std::fs::symlink_metadata(&path)
                        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?,
                ) || super::folder::reparse(
                    &std::fs::symlink_metadata(storage)
                        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?,
                ) {
                    return Err("ATTACHMENT_PATH_UNSAFE".into());
                }
                let lock = storage.join(".les-native-upload.lock");
                if std::fs::symlink_metadata(&lock)
                    .is_ok_and(|m| super::folder::reparse(&m) || !m.is_file())
                {
                    return Err("ATTACHMENT_PATH_UNSAFE".into());
                }
                let lease = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(lock)
                    .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
                lease.try_lock().map_err(|_| "ATTACHMENT_QUEUE_BUSY")?;
                for entry in std::fs::read_dir(&path).map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")? {
                    let entry = entry.map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
                    let file = entry.file_name();
                    let Some((id, extension)) = file
                        .to_str()
                        .and_then(|n| n.strip_prefix("native-read_"))
                        .and_then(|n| n.split_once('.'))
                    else {
                        continue;
                    };
                    if id.len() == 12
                        && id.bytes().all(|b| b.is_ascii_hexdigit())
                        && extension.len() <= 12
                        && !extension.is_empty()
                        && extension.bytes().all(|b| b.is_ascii_alphanumeric())
                    {
                        let meta = std::fs::symlink_metadata(entry.path())
                            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
                        if meta.is_file() && !super::folder::reparse(&meta) {
                            std::fs::remove_file(entry.path())
                                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
                        }
                    }
                }
                Ok(lease)
            })
            .await
            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE".to_owned())?
        })
        .await?;
    Ok(directory)
}
pub(super) struct Temporary(pub PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        if std::fs::remove_file(&self.0).is_err() {
            // Tokio may still be completing a cancelled Windows write. Retry only
            // this owned file after its handle closes; never block the HTTP thread.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let path = self.0.clone();
                runtime.spawn_blocking(move||{
                    for _ in 0..40 {
                        match std::fs::remove_file(&path){Ok(())=>return,Err(e)if e.kind()==std::io::ErrorKind::NotFound=>return,_=>{}}
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    eprintln!("{}",json!({"event":"attachment_temporary_cleanup_failed","code":"ATTACHMENT_STORE_UNAVAILABLE"}));
                });
            }
        }
    }
}
fn name(raw: &str) -> Result<String> {
    if raw.chars().count() > 255
        || raw.trim().is_empty()
        || raw
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
        || raw.ends_with([' ', '.'])
    {
        return Err("ATTACHMENT_NAME_INVALID".into());
    }
    let stem = raw.split('.').next().unwrap_or("").to_ascii_uppercase();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&stem.as_str())
    {
        return Err("ATTACHMENT_NAME_INVALID".into());
    }
    Ok(raw.to_owned())
}
async fn receive(context: &Context, request: Request) -> Result<(Temporary, String)> {
    let settings = Settings::load(&context.state_root)?;
    let limit = settings
        .get("MAX_UPLOAD_MB", "100")
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0 && *v <= 512)
        .ok_or("ATTACHMENT_POLICY_INVALID")?
        * 1024
        * 1024;
    let boundary = multer::parse_boundary(
        request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .ok_or("ATTACHMENT_MULTIPART_INVALID")?,
    )
    .map_err(|_| "ATTACHMENT_MULTIPART_INVALID")?;
    let constraints = Constraints::new()
        .allowed_fields(vec!["file", "candidate_acceptance"])
        .size_limit(
            SizeLimit::new()
                .whole_stream(limit + 65536)
                .for_field("file", limit)
                .for_field("candidate_acceptance", 16),
        );
    let mut multipart = multer::Multipart::with_constraints(
        request.into_body().into_data_stream(),
        boundary,
        constraints,
    );
    let directory = scratch(context).await?;
    let mut temporary = None;
    let mut original = None;
    let mut acceptance = false;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| multipart_error(&e))?
    {
        if field.name() == Some("candidate_acceptance") {
            if acceptance {
                return Err("ATTACHMENT_MULTIPART_INVALID".into());
            }
            let value = field.text().await.map_err(|e| multipart_error(&e))?;
            if !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "off" | "no" | "f" | "n"
            ) {
                return Err("CANDIDATE_ACCEPTANCE_REQUIRED".into());
            }
            acceptance = true;
            continue;
        }
        if field.name() != Some("file") || temporary.is_some() {
            return Err("ATTACHMENT_MULTIPART_INVALID".into());
        }
        let filename = name(field.file_name().unwrap_or("upload.bin"))?;
        let suffix = Path::new(&filename)
            .extension()
            .and_then(|s| s.to_str())
            .ok_or("ATTACHMENT_FORMAT_UNSUPPORTED")?
            .to_ascii_lowercase();
        let allowed = settings.get(
            "RAG_UPLOAD_SUFFIXES",
            ".pdf,.docx,.doc,.eml,.emlx,.msg,.xlsx,.xls,.csv,.json,.jsonl,.md,.txt",
        );
        if !["png", "jpg", "jpeg", "webp"].contains(&suffix.as_str())
            && !allowed.split(',').any(|s| {
                s.trim()
                    .trim_start_matches('.')
                    .eq_ignore_ascii_case(&suffix)
            })
        {
            return Err("ATTACHMENT_FORMAT_UNSUPPORTED".into());
        }
        let path = directory.join(format!(
            "native-{}.{}",
            les_core::chat_attachments::new_id()?,
            suffix
        ));
        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await
            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
        let temp = Temporary(path);
        let mut size = 0u64;
        while let Some(bytes) = field.chunk().await.map_err(|e| multipart_error(&e))? {
            size += bytes.len() as u64;
            if size > limit {
                return Err("ATTACHMENT_UPLOAD_LIMIT".into());
            }
            output
                .write_all(&bytes)
                .await
                .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
        }
        output
            .flush()
            .await
            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
        drop(output);
        if size == 0 {
            return Err("ATTACHMENT_EMPTY".into());
        }
        temporary = Some(temp);
        original = Some(filename);
    }
    let temp = temporary.ok_or("ATTACHMENT_MULTIPART_INVALID")?;
    Ok((temp, original.ok_or("ATTACHMENT_MULTIPART_INVALID")?))
}
fn multipart_error(error: &multer::Error) -> String {
    match error {
        multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. } => {
            "ATTACHMENT_UPLOAD_LIMIT"
        }
        _ => "ATTACHMENT_MULTIPART_INVALID",
    }
    .into()
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    perform(context, request, None, StatusCode::OK).await
}
async fn perform(
    context: Arc<Context>,
    request: Request,
    caller: Option<String>,
    status: StatusCode,
) -> Response {
    let result = async {
        let _slot = context
            .upload_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "ATTACHMENT_QUEUE_BUSY")?;
        let key = if caller.is_some() {
            if request.headers().get_all("idempotency-key").iter().count() > 1 {
                return Err("ATTACHMENT_KEY_INVALID".into());
            }
            request
                .headers()
                .get("idempotency-key")
                .map(|v| v.to_str().map(str::to_owned))
                .transpose()
                .map_err(|_| "ATTACHMENT_KEY_INVALID")?
        } else {
            None
        };
        if request.uri().path() == FOLDER {
            return super::folder::receive(
                context,
                request,
                caller.ok_or("ATTACHMENT_REQUEST_INVALID")?,
                key,
            )
            .await;
        }
        let (temp, name) = receive(&context, request).await?;
        publish(context, &temp, name, caller, key).await
    };
    match tokio::time::timeout(Duration::from_secs(180), result).await {
        Ok(Ok(value)) => crate::json_response(status, value, "rust.attachments"),
        Ok(Err(code)) => failure(&code),
        Err(_) => failure("ATTACHMENT_UPLOAD_TIMEOUT"),
    }
}
pub(super) async fn publish(
    context: Arc<Context>,
    temp: &Temporary,
    name: String,
    caller: Option<String>,
    key: Option<String>,
) -> Result<Value> {
    if let Some(caller) = caller {
        let path = temp.0.clone();
        let label = name.clone();
        let claim = tokio::task::spawn_blocking(move || {
            les_core::chat_attachments::claim(&path, &label, &caller, key.as_deref(), "file")
        })
        .await
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")??;
        let id = claim.ident.clone();
        super::prepare_asset(context, temp.0.clone(), name, Some(id), Some(claim)).await
    } else {
        super::prepare(context, temp.0.clone(), name, None).await
    }
}
pub async fn handle_public(context: Arc<Context>, request: Request) -> Response {
    let public = request.uri().path() == PUBLIC;
    let legacy = request.uri().path() == LEGACY;
    let (cors, caller) =
        match crate::auth::authorize_caller(&context, request.headers(), !public).await {
            Ok(v) => v,
            Err(r) => return r,
        };
    let mut result = if !crate::auth::origin_allowed(request.headers(), &cors) {
        crate::error(StatusCode::FORBIDDEN, "Источник загрузки не разрешён.")
    } else if request.method() != "POST" {
        let mut r = crate::error(
            StatusCode::METHOD_NOT_ALLOWED,
            "Для загрузки используйте POST.",
        );
        r.headers_mut().insert("allow", "POST".parse().unwrap());
        r
    } else {
        let mode = if legacy {
            reqwest::Url::parse(&format!("http://localhost{}", request.uri()))
                .ok()
                .and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "mode")
                        .map(|(_, v)| v.into_owned())
                })
                .unwrap_or_else(|| "read".into())
        } else {
            "read".into()
        };
        match mode.as_str() {
            "read" | "quick" => {
                perform(
                    context,
                    request,
                    Some(caller),
                    if public {
                        StatusCode::CREATED
                    } else {
                        StatusCode::OK
                    },
                )
                .await
            }
            "index" => failure("ATTACHMENT_INDEX_PENDING"),
            _ => failure("ATTACHMENT_MODE_INVALID"),
        }
    };
    result.headers_mut().extend(cors);
    result
}
