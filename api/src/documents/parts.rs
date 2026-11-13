//! Immutable Word media, embedded files and drawing structure, pinned to a catalog revision.
use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, &'static str>;
const BAD: &str = "SOURCE_PART_INVALID";
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn route(path: &str) -> Option<(&str, Option<&str>)> {
    let rest = path.strip_prefix("/api/documents/by-id/")?;
    let (id, part) = rest.split_once("/parts")?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    if part.is_empty() {
        Some((id, None))
    } else {
        part.strip_prefix('/')
            .filter(|s| hash(s))
            .map(|s| (id, Some(s)))
    }
}
fn root(context: &Context, doc: &Value, expected: &str) -> Result<PathBuf> {
    let source = doc["file_hash"]
        .as_str()
        .filter(|s| hash(s))
        .ok_or("SOURCE_REVISION_REQUIRED")?;
    if !expected.is_empty() && source != expected {
        return Err("SOURCE_REVISION_CHANGED");
    }
    let state = context.state_root.canonicalize().map_err(|_| BAD)?;
    let path = state.join("storage/docx-parts").join(source);
    if path
        .canonicalize()
        .map_err(|_| "SOURCE_PARTS_UNAVAILABLE")?
        != path
    {
        return Err(BAD);
    }
    Ok(path)
}
fn open_hash(root: &Path, sha: &str, max: u64) -> Result<(File, u64)> {
    if !hash(sha) {
        return Err(BAD);
    }
    let path = root.join(sha);
    let meta = fs::symlink_metadata(&path).map_err(|_| "SOURCE_PARTS_UNAVAILABLE")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > max {
        return Err(BAD);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    let mut file = options.open(path).map_err(|_| BAD)?;
    let size = file.metadata().map_err(|_| BAD)?.len();
    if size > max {
        return Err(BAD);
    }
    if les_core::pdf_source::fingerprint(&mut file).map_err(|_| BAD)? != sha {
        return Err("SOURCE_PART_CORRUPT");
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
    Ok((file, size))
}
pub(crate) fn manifest(context: &Context, doc: &Value, expected: &str) -> Result<Value> {
    let root = root(context, doc, expected)?;
    let pointer = root.join("manifest.json");
    let meta = fs::symlink_metadata(&pointer).map_err(|_| "SOURCE_PARTS_UNAVAILABLE")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 {
        return Err(BAD);
    }
    let pointer: Value =
        serde_json::from_slice(&fs::read(pointer).map_err(|_| BAD)?).map_err(|_| BAD)?;
    let sha = pointer["sha256"].as_str().filter(|s| hash(s)).ok_or(BAD)?;
    let (mut file, size) = open_hash(&root, sha, 8 * 1024 * 1024)?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.read_to_end(&mut bytes).map_err(|_| BAD)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| BAD)?;
    if value["schema"] != "les.docx-parts.v1" || value["source_sha256"] != doc["file_hash"] {
        return Err(BAD);
    }
    let entries = value["parts"].as_array_mut().ok_or(BAD)?;
    if entries.len() > 4096 {
        return Err(BAD);
    }
    let mut images = 0;
    let mut objects = 0;
    let mut structures = 0;
    for entry in entries {
        let id = entry["sha256"]
            .as_str()
            .filter(|s| hash(s))
            .ok_or(BAD)?
            .to_owned();
        let label = match entry["kind"].as_str() {
            Some("image") => {
                images += 1;
                format!("Рисунок {images}")
            }
            Some("structure") => {
                structures += 1;
                format!("Структура рисунка {structures}")
            }
            _ => {
                objects += 1;
                format!("Вложение {objects}")
            }
        };
        entry["label"] = json!(label);
        entry["url"] = json!(url(doc, &id));
        if let Some(preview) = entry["preview_sha256"].as_str().filter(|s| hash(s)) {
            entry["preview_url"] = json!(url(doc, preview));
        }
        if let Some(views) = entry["views"].as_array_mut() {
            for view in views {
                let sha = view["sha256"]
                    .as_str()
                    .filter(|s| hash(s))
                    .ok_or(BAD)?
                    .to_owned();
                view["url"] = json!(url(doc, &sha));
            }
        }
    }
    value["source_url"] = json!(url(doc, doc["file_hash"].as_str().ok_or(BAD)?));
    value["source_state"] = doc["status"].clone();
    Ok(value)
}
pub(crate) fn url(doc: &Value, sha: &str) -> String {
    let mut uri = reqwest::Url::parse("http://localhost/api/documents/by-id/").unwrap();
    uri.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(doc["id"].as_str().unwrap_or(""))
        .push("parts")
        .push(sha);
    uri.query_pairs_mut()
        .append_pair("source_sha256", doc["file_hash"].as_str().unwrap_or(""));
    format!("{}?{}", uri.path(), uri.query().unwrap_or(""))
}
pub(crate) fn open(
    context: &Context,
    doc: &Value,
    expected: &str,
    sha: &str,
) -> Result<(File, u64, Value)> {
    open_member(context, doc, expected, sha, None)
}
pub(crate) fn open_member(
    context: &Context,
    doc: &Value,
    expected: &str,
    sha: &str,
    member: Option<&str>,
) -> Result<(File, u64, Value)> {
    let manifest = manifest(context, doc, expected)?;
    let mut entry = if member.is_none() && sha == doc["file_hash"].as_str().ok_or(BAD)? {
        json!({"kind":"source","member":"document.docx"})
    } else {
        manifest["parts"]
            .as_array()
            .ok_or(BAD)?
            .iter()
            .filter(|v| member.is_none_or(|name| v["member"] == name))
            .find(|v| {
                v["sha256"] == sha
                    || v["preview_sha256"] == sha
                    || v["views"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v["sha256"] == sha))
            })
            .cloned()
            .ok_or(if member.is_some() {
                "SOURCE_PART_IDENTITY_MISMATCH"
            } else {
                "SOURCE_PART_NOT_FOUND"
            })?
    };
    if let Some(view) = entry["views"]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["sha256"] == sha))
        .cloned()
    {
        entry["selected_view"] = view;
    }
    let (file, size) = open_hash(&root(context, doc, expected)?, sha, 64 * 1024 * 1024)?;
    Ok((file, size, entry))
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match auth::authorize(&context, request.headers(), false).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (id, part) = route(request.uri().path()).unwrap();
    let id = match percent_encoding::percent_decode_str(id).decode_utf8() {
        Ok(v) => v.into_owned(),
        Err(_) => return crate::error(StatusCode::BAD_REQUEST, "Некорректная ссылка на источник."),
    };
    let part = part.map(str::to_owned);
    let head = request.method() == "HEAD";
    let uri = reqwest::Url::parse(&format!("http://localhost{}", request.uri())).unwrap();
    let expected = uri
        .query_pairs()
        .find(|(k, _)| k == "source_sha256")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let range = if request.headers().contains_key("if-range") {
        None
    } else {
        request
            .headers()
            .get("range")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let owner = context.clone();
    let opened =
        tokio::task::spawn_blocking(move || -> Result<(Option<(File, u64, Value)>, Value)> {
            let doc = catalog_document(&owner, &json!({"doc_id":id}))?;
            match part {
                Some(sha) => Ok((Some(open(&owner, &doc, &expected, &sha)?), Value::Null)),
                None => Ok((None, manifest(&owner, &doc, &expected)?)),
            }
        })
        .await;
    let (opened, value) = match opened {
        Ok(Ok(v)) => v,
        result => {
            let code = match result {
                Ok(Err(e)) => e,
                _ => BAD,
            };
            let status = if code == "SOURCE_REVISION_CHANGED" {
                StatusCode::CONFLICT
            } else if matches!(
                code,
                "SOURCE_PART_NOT_FOUND" | "SOURCE_DOCUMENT_NOT_FOUND" | "SOURCE_PARTS_UNAVAILABLE"
            ) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            let mut reply = crate::json_response(
                status,
                json!({"code":code,"detail":"Не удалось открыть эту часть источника. Повторите чтение текущей редакции."}),
                "rust.document_parts",
            );
            reply.headers_mut().extend(cors);
            return reply;
        }
    };
    let Some((mut file, length, entry)) = opened else {
        let mut response = crate::json_response(StatusCode::OK, value, "rust.document_parts");
        response.headers_mut().extend(cors);
        if head {
            *response.body_mut() = Body::empty();
        }
        return response;
    };
    let range = match byte_range(range.as_deref(), length) {
        Ok(v) => v,
        Err(_) => {
            return crate::error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "Некорректный диапазон источника.",
            )
        }
    };
    let mut magic = [0; 12];
    let _ = file.read(&mut magic);
    let mime = if magic.starts_with(b"%PDF-") {
        "application/pdf"
    } else if magic.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if magic.starts_with(b"\xff\xd8") {
        "image/jpeg"
    } else if entry["kind"] == "structure" {
        "application/json"
    } else {
        "application/octet-stream"
    };
    let (start, count) = range.map(|(a, b)| (a, b - a + 1)).unwrap_or((0, length));
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    tokio::task::spawn_blocking(move || {
        if head {
            return;
        }
        if let Err(e) = file.seek(SeekFrom::Start(start)) {
            let _ = sender.blocking_send(Err(e));
            return;
        }
        let mut remaining = count;
        let mut buffer = vec![0; 64 * 1024];
        while remaining > 0 && !sender.is_closed() {
            let take = remaining.min(buffer.len() as u64) as usize;
            match file.read(&mut buffer[..take]) {
                Ok(0) => {
                    let _ =
                        sender.blocking_send(Err(std::io::Error::other("SOURCE_PART_TRUNCATED")));
                    return;
                }
                Ok(n) => {
                    remaining -= n as u64;
                    if sender.blocking_send(Ok(buffer[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = sender.blocking_send(Err(e));
                    return;
                }
            }
        }
    });
    let body = Body::from_stream(stream::unfold(receiver, |mut r| async move {
        r.recv().await.map(|v| (v, r))
    }));
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static(mime));
    headers.insert("content-length", count.to_string().parse().unwrap());
    headers.insert("accept-ranges", HeaderValue::from_static("bytes"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert(
        "x-les-handler",
        HeaderValue::from_static("rust.document_parts"),
    );
    if let Some((a, b)) = range {
        headers.insert(
            "content-range",
            format!("bytes {a}-{b}/{length}").parse().unwrap(),
        );
    }
    let name = entry["member"]
        .as_str()
        .unwrap_or("document-part")
        .rsplit('/')
        .next()
        .unwrap_or("document-part");
    let name = if mime == "image/png" && !name.to_ascii_lowercase().ends_with(".png") {
        format!("{name}.png")
    } else {
        name.to_owned()
    };
    let encoded = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
    if let Ok(v) = format!("inline; filename=\"document-part\"; filename*=UTF-8''{encoded}").parse()
    {
        headers.insert("content-disposition", v);
    }
    headers.extend(cors);
    (
        if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        },
        headers,
        body,
    )
        .into_response()
}
