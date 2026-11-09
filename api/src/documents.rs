//! Original bytes resolved through the catalog and checked on the streamed handle.
pub mod browse;
use crate::{auth, Context};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::stream;
use serde_json::{json, Value};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
pub fn route(path: &str) -> Option<&str> {
    path.strip_prefix("/api/documents/by-id/")?
        .strip_suffix("/raw")
        .filter(|id| !id.is_empty() && !id.contains('/'))
}
fn catalog_document(context: &Context, args: &Value) -> Result<Value, &'static str> {
    let read = |args| {
        let value = crate::library::catalog(&context.database, "library_document", args);
        if value["status"] != "completed" {
            return Err("SOURCE_CATALOG_UNAVAILABLE");
        }
        Ok(value["value"].clone())
    };
    let mut doc = read(args.clone())?;
    if doc.is_null()
        && ["dataset_id", "file_name"]
            .iter()
            .all(|key| args[*key].as_str().is_some_and(|v| !v.is_empty()))
    {
        // Legacy citations may contain a fragment ID. Use both exact fields
        // or refuse; a basename or recent revision is never guessed.
        let mut exact = args.clone();
        exact["doc_id"] = json!("");
        doc = read(exact)?;
    }
    if doc.is_null() {
        return Err("SOURCE_DOCUMENT_NOT_FOUND");
    }
    Ok(doc)
}

fn verified_file(
    context: &Context,
    doc: &Value,
    expected: &str,
) -> Result<(File, u64), &'static str> {
    let expected = if expected.is_empty() {
        doc["file_hash"].as_str().unwrap_or("")
    } else {
        expected
    };
    if !expected.is_empty()
        && (expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("SOURCE_REVISION_REQUIRED");
    }
    if doc["file_hash"]
        .as_str()
        .is_some_and(|h| h.len() == 64 && !h.eq_ignore_ascii_case(expected))
    {
        return Err("SOURCE_REVISION_CHANGED");
    }
    let path = les_core::document_source::resolve(
        doc["source_path"].as_str().unwrap_or(""),
        doc["dataset_id"]
            .as_str()
            .ok_or("SOURCE_IDENTITY_INVALID")?,
        doc["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?,
        &context.state_root.join("storage/datasets"),
    )?;
    let mut file = File::open(path).map_err(|_| "SOURCE_UNAVAILABLE")?;
    let actual = les_core::pdf_source::fingerprint(&mut file).map_err(|e| {
        if e == "PDF_REVISION_CHANGED" {
            "SOURCE_REVISION_CHANGED"
        } else {
            "SOURCE_READ_FAILED"
        }
    })?;
    if !expected.is_empty() && !actual.eq_ignore_ascii_case(expected) {
        return Err("SOURCE_REVISION_CHANGED");
    }
    let length = file.metadata().map_err(|_| "SOURCE_READ_FAILED")?.len();
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "SOURCE_READ_FAILED")?;
    Ok((file, length))
}
fn byte_range(value: Option<&str>, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = value else { return Ok(None) };
    // Multipart ranges may legally receive the full representation. Never
    // allocate the complete file to assemble multipart output.
    if value.contains(',') {
        return Ok(None);
    }
    let (a, b) = value
        .strip_prefix("bytes=")
        .ok_or(())?
        .split_once('-')
        .ok_or(())?;
    if length == 0 {
        return Err(());
    }
    let (start, end) = if a.is_empty() {
        let suffix = b.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (length.saturating_sub(suffix), length - 1)
    } else {
        let start = a.parse::<u64>().map_err(|_| ())?;
        let end = if b.is_empty() {
            length - 1
        } else {
            b.parse::<u64>().map_err(|_| ())?.min(length - 1)
        };
        (start, end)
    };
    if start >= length || start > end {
        return Err(());
    }
    Ok(Some((start, end)))
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match auth::authorize(&context, request.headers(), false).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let head = request.method() == "HEAD";
    let range = if request.headers().contains_key("if-range") {
        None
    } else {
        request
            .headers()
            .get("range")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let id = route(request.uri().path()).unwrap_or("");
    let id = match percent_encoding::percent_decode_str(id).decode_utf8() {
        Ok(v) => v.into_owned(),
        Err(_) => return crate::error(StatusCode::BAD_REQUEST, "Некорректная ссылка на документ."),
    };
    let url = match reqwest::Url::parse(&format!("http://localhost{}", request.uri())) {
        Ok(v) => v,
        Err(_) => return crate::error(StatusCode::BAD_REQUEST, "Некорректная ссылка на документ."),
    };
    let mut args = json!({"doc_id":id});
    for (key, value) in url.query_pairs() {
        if ["dataset_id", "doc_name", "source_sha256"].contains(&key.as_ref()) {
            if value.len() > 2000 {
                return crate::error(StatusCode::BAD_REQUEST, "Некорректная ссылка на документ.");
            }
            args[key.as_ref()] = json!(value);
        }
    }
    if let Some(name) = args.get("doc_name").cloned() {
        args["file_name"] = name;
    }
    let opened =
        tokio::task::spawn_blocking(move || -> Result<(File, u64, String), &'static str> {
            let doc = catalog_document(&context, &args)?;
            let name = doc["file_name"].as_str().ok_or("SOURCE_IDENTITY_INVALID")?;
            let (file, length) =
                verified_file(&context, &doc, args["source_sha256"].as_str().unwrap_or(""))?;
            Ok((
                file,
                length,
                name.rsplit(['/', '\\']).next().unwrap_or("document").into(),
            ))
        })
        .await;
    let (mut file, length, name) = match opened {
        Ok(Ok(v)) => v,
        result => {
            let code = match result {
                Ok(Err(e)) => e,
                _ => "SOURCE_READ_FAILED",
            };
            let status = if code == "SOURCE_DOCUMENT_NOT_FOUND" {
                StatusCode::NOT_FOUND
            } else if code == "SOURCE_REVISION_CHANGED" {
                StatusCode::CONFLICT
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            return crate::json_response(
                status,
                json!({"code":code,"detail":"Не удалось открыть эту редакцию источника. Проверьте оригинал или повторите поиск по текущей версии."}),
                "rust.documents",
            );
        }
    };
    let range = match byte_range(range.as_deref(), length) {
        Ok(v) => v,
        Err(_) => {
            let mut response = crate::error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "Запрошенная часть файла недоступна.",
            );
            response.headers_mut().insert(
                "content-range",
                format!("bytes */{length}").parse().unwrap(),
            );
            return response;
        }
    };
    let (start, count) = range.map(|(s, e)| (s, e - s + 1)).unwrap_or((0, length));
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
        let mut buffer = vec![0u8; 64 * 1024];
        while remaining > 0 && !sender.is_closed() {
            let take = (remaining as usize).min(buffer.len());
            let n = match file.read(&mut buffer[..take]) {
                Ok(0) => {
                    let _ = sender
                        .blocking_send(Err(std::io::Error::other("SOURCE_CHANGED_DURING_READ")));
                    return;
                }
                Ok(n) => n,
                Err(e) => {
                    let _ = sender.blocking_send(Err(e));
                    return;
                }
            };
            if n as u64 > remaining {
                let _ =
                    sender.blocking_send(Err(std::io::Error::other("SOURCE_CHANGED_DURING_READ")));
                return;
            }
            remaining -= n as u64;
            if sender.blocking_send(Ok(buffer[..n].to_vec())).is_err() {
                return;
            }
        }
    });
    let mime = if name.to_ascii_lowercase().ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    let body = Body::from_stream(stream::unfold(receiver, |mut r| async move {
        r.recv().await.map(|v| (v, r))
    }));
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static(mime));
    headers.insert("x-les-handler", HeaderValue::from_static("rust.documents"));
    headers.insert("content-length", count.to_string().parse().unwrap());
    headers.insert("accept-ranges", HeaderValue::from_static("bytes"));
    if let Some((start, end)) = range {
        headers.insert(
            "content-range",
            format!("bytes {start}-{end}/{length}").parse().unwrap(),
        );
    }
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    let name = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
    if let Ok(v) = format!("inline; filename=\"document\"; filename*=UTF-8''{name}").parse() {
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_ranges_preserve_browser_seeking_and_empty_file_rules() {
        for (request, expected) in [
            ("bytes=0-3", (0, 3)),
            ("bytes=5-", (5, 9)),
            ("bytes=-3", (7, 9)),
            ("bytes=8-99", (8, 9)),
        ] {
            assert_eq!(byte_range(Some(request), 10), Ok(Some(expected)));
        }
        for request in ["bytes=10-", "bytes=5-3", "bytes=-0", "bytes=0-x"] {
            assert!(byte_range(Some(request), 10).is_err());
        }
        assert!(byte_range(Some("bytes=0-"), 0).is_err());
        assert_eq!(byte_range(None, 0), Ok(None));
    }
}
