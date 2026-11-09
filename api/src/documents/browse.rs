//! Source-panel reads for a standalone public chat.
use super::*;
use axum::http::{header, Method, Uri};

enum Route<'a> {
    Datasets,
    Metadata(&'a str),
    Preview(&'a str, &'a str),
}
fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/documents/datasets" {
        return Some(Route::Datasets);
    }
    let path = path.strip_prefix("/api/documents/by-id/")?;
    if !path.is_empty() && !path.contains('/') {
        return Some(Route::Metadata(path));
    }
    let (id, page) = path.split_once("/pdf-contour/pages/")?;
    let page = page.strip_suffix("/preview")?;
    (!id.is_empty() && !id.contains('/') && !page.is_empty() && !page.contains('/'))
        .then_some(Route::Preview(id, page))
}
pub fn handles(request: &Request) -> bool {
    request.method() == Method::GET && route(request.uri().path()).is_some()
}
pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let cors = match auth::authorize(&context, request.headers(), false).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let uri = request.uri().clone();
    let mut response = match context.workspace_slots.clone().try_acquire_owned() {
        Ok(permit) => tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read(&context, &uri).unwrap_or_else(|code| failed(&code))
        })
        .await
        .unwrap_or_else(|_| failed("SOURCE_READ_FAILED")),
        Err(_) => failed("SOURCE_QUEUE_BUSY"),
    };
    response.headers_mut().extend(cors);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
fn read(context: &Context, uri: &Uri) -> Result<Response, String> {
    let route = route(uri.path()).ok_or("SOURCE_DOCUMENT_NOT_FOUND")?;
    let query = uri.query().unwrap_or("");
    if query.len() > 12000 {
        return Err("SOURCE_QUERY_INVALID".into());
    }
    let fields = reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .map_err(|_| "SOURCE_QUERY_INVALID")?
        .query_pairs()
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    let param = |key| fields.get(key).map(String::as_str).unwrap_or("");
    if matches!(route, Route::Datasets) {
        let limit = if param("limit").is_empty() {
            200
        } else {
            param("limit")
                .parse::<usize>()
                .ok()
                .filter(|v| (1..=1000).contains(v))
                .ok_or("SOURCE_QUERY_INVALID")?
        };
        let value = crate::library::catalog(&context.database, "library_datasets", json!({}));
        if value["status"] != "completed" {
            return Err("SOURCE_CATALOG_UNAVAILABLE".into());
        }
        let q = param("q").trim().to_lowercase();
        let rows = value["value"]["datasets"]
            .as_array()
            .ok_or("SOURCE_CATALOG_UNAVAILABLE")?
            .iter()
            .filter(|row| {
                row["name"]
                    .as_str()
                    .is_some_and(|s| s.to_lowercase().contains(&q))
            })
            .take(limit)
            .collect::<Vec<_>>();
        return Ok(crate::json_response(
            StatusCode::OK,
            json!({"datasets":rows}),
            "rust.documents",
        ));
    }
    let (id, page) = match route {
        Route::Metadata(id) => (id, None),
        Route::Preview(id, page) => (
            id,
            Some(
                page.parse::<u64>()
                    .ok()
                    .filter(|v| *v > 0)
                    .ok_or("PDF_PAGE_REQUIRED")?,
            ),
        ),
        Route::Datasets => unreachable!(),
    };
    let id = percent_encoding::percent_decode_str(id)
        .decode_utf8()
        .map_err(|_| "SOURCE_QUERY_INVALID")?;
    let digest = param("source_sha256");
    let args = json!({"doc_id":id,"dataset_id":param("dataset_id"),"file_name":param("doc_name")});
    let mut doc = catalog_document(context, &args)?;
    let Some(page) = page else {
        if !digest.is_empty() {
            verified_file(context, &doc, digest)?;
        }
        doc.as_object_mut()
            .ok_or("SOURCE_CATALOG_UNAVAILABLE")?
            .remove("source_path");
        return Ok(crate::json_response(
            StatusCode::OK,
            json!({"document":doc}),
            "rust.documents",
        ));
    };
    let width = if param("width").is_empty() {
        1200
    } else {
        param("width")
            .parse::<u64>()
            .ok()
            .filter(|v| (320..=1800).contains(v))
            .ok_or("PDF_PREVIEW_WIDTH_INVALID")?
    };
    let bbox = param("bbox").trim();
    let bbox = if bbox.is_empty() {
        Value::Null
    } else {
        if bbox.len() > 160 {
            return Err("PDF_PREVIEW_BBOX_INVALID".into());
        }
        let values = bbox
            .split(',')
            .map(|v| {
                v.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or("PDF_PREVIEW_BBOX_INVALID")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if values.len() != 4 {
            return Err("PDF_PREVIEW_BBOX_INVALID".into());
        }
        json!(values)
    };
    let result = crate::pdf_text::supervised_source(
        json!({"kind":"source_preview","database":context.database,
        "content_root":context.state_root.join("storage/datasets"),"source":{"doc_id":doc["id"],"dataset_id":doc["dataset_id"],
            "doc_name":doc["file_name"],"source_sha256":digest,"page":page,"width":width,"bbox":bbox}}),
    )?;
    let bytes =
        les_core::pdf_source::decode_png(result["png"].as_str().ok_or("PDF_PREVIEW_FAILED")?)?;
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("inline; filename=\"page-{page}.png\"")
            .parse()
            .unwrap(),
    );
    headers.insert("x-les-handler", HeaderValue::from_static("rust.documents"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}
fn failed(code: &str) -> Response {
    let (status, detail) = match code {
        "SOURCE_DOCUMENT_NOT_FOUND" | "PDF_DOCUMENT_NOT_FOUND" => {
            (StatusCode::NOT_FOUND, "Документ не найден.")
        }
        "SOURCE_REVISION_CHANGED" | "PDF_REVISION_CHANGED" => (
            StatusCode::CONFLICT,
            "Редакция документа изменилась. Откройте текущий источник или повторите поиск.",
        ),
        "PDF_PAGE_REQUIRED"
        | "PDF_PAGE_OUT_OF_RANGE"
        | "PDF_PREVIEW_WIDTH_INVALID"
        | "PDF_PREVIEW_BBOX_INVALID"
        | "SOURCE_QUERY_INVALID" => (
            StatusCode::BAD_REQUEST,
            "Некорректная страница или область предпросмотра.",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось открыть источник. Проверьте доступность оригинала и повторите действие.",
        ),
    };
    crate::json_response(
        status,
        json!({"code":code,"detail":detail}),
        "rust.documents",
    )
}
