//! Streaming multipart intake. Parsing never runs in the HTTP task.
use super::*;
use multer::{Constraints, SizeLimit};
use tokio::io::AsyncWriteExt;
pub const PATH: &str = "/api/light/internal-attachments/upload";
struct Temporary(PathBuf);
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
async fn receive(context: Arc<Context>, request: Request) -> Result<Value> {
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
    let directory = context.state_root.join("storage/upload-tmp");
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
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
            if value != "false" {
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
        let temp = Temporary(directory.join(format!(
            "{}.{}",
            les_core::chat_attachments::new_id()?,
            suffix
        )));
        // The handle is declared after its cleanup guard, so cancellation closes it first.
        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp.0)
            .await
            .map_err(|_| "ATTACHMENT_STORE_UNAVAILABLE")?;
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
    super::prepare(
        context,
        temp.0.clone(),
        original.ok_or("ATTACHMENT_MULTIPART_INVALID")?,
        None,
    )
    .await
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
    match tokio::time::timeout(Duration::from_secs(180), receive(context, request)).await {
        Ok(Ok(value)) => crate::json_response(StatusCode::OK, value, "rust.attachments"),
        Ok(Err(code)) => failure(&code),
        Err(_) => failure("ATTACHMENT_UPLOAD_TIMEOUT"),
    }
}
