//! Serve the compiled frontend from its verified receipt. Requests never open a file.
use crate::{auth, json_response, Context};
use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::Response,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::Path,
};

const RECEIPT: &str = "build-receipt.json";
const INVALID: &str = "FRONTEND_ASSET_INVALID";
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self' ipc: http://ipc.localhost; frame-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'self'";
const MAX_FILE: u64 = 5_000_000;
const MAX_TOTAL: usize = 32 * 1024 * 1024;

struct Asset {
    bytes: Bytes,
    content_type: &'static str,
    etag: HeaderValue,
}

pub struct Frontend {
    assets: HashMap<String, Asset>,
}

fn reparse(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn read_asset(root: &Path, name: &str, limit: u64) -> Result<Vec<u8>, &'static str> {
    let mut path = root.to_owned();
    for part in name.split('/') {
        if part.is_empty()
            || part.ends_with(['.', ' '])
            || part.contains(['\\', ':', '?', '*', '<', '>', '|', '"'])
            || part.chars().any(char::is_control)
        {
            return Err(INVALID);
        }
        path.push(part);
        let metadata = fs::symlink_metadata(&path).map_err(|_| INVALID)?;
        if reparse(&metadata) {
            return Err(INVALID);
        }
    }
    let path = path.canonicalize().map_err(|_| INVALID)?;
    if !path.starts_with(root) {
        return Err(INVALID);
    }
    let file = File::open(path).map_err(|_| INVALID)?;
    let metadata = file.metadata().map_err(|_| INVALID)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(INVALID);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > limit {
        return Err(INVALID);
    }
    Ok(bytes)
}

fn mime(name: &str) -> Option<&'static str> {
    if name == RECEIPT {
        return Some("application/json");
    }
    match Path::new(name).extension()?.to_str()? {
        "html" => Some("text/html; charset=utf-8"),
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "woff2" => Some("font/woff2"),
        "txt" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
}

impl Asset {
    fn new(name: &str, bytes: Vec<u8>, digest: &str) -> Result<Self, &'static str> {
        Ok(Self {
            bytes: Bytes::from(bytes),
            content_type: mime(name).ok_or(INVALID)?,
            etag: format!("\"{digest}\"").parse().map_err(|_| INVALID)?,
        })
    }
}

impl Frontend {
    pub fn load(path: &Path) -> Result<Self, &'static str> {
        let metadata = fs::symlink_metadata(path).map_err(|_| INVALID)?;
        if !path.is_absolute() || !metadata.is_dir() || reparse(&metadata) {
            return Err(INVALID);
        }
        let root = path.canonicalize().map_err(|_| INVALID)?;
        let receipt = read_asset(&root, RECEIPT, 128 * 1024)?;
        let manifest: Value = serde_json::from_slice(&receipt).map_err(|_| INVALID)?;
        let files = manifest["files"].as_object().ok_or(INVALID)?;
        if manifest["schema"] != "les.frontend.v1"
            || !files.contains_key("index.html")
            || files.contains_key(RECEIPT)
            || files.len() > 256
        {
            return Err(INVALID);
        }
        let mut total = receipt.len();
        let mut assets = HashMap::new();
        for (name, hash) in files {
            let digest = hash.as_str().ok_or(INVALID)?;
            let bytes = read_asset(&root, name, MAX_FILE)?;
            total += bytes.len();
            if total > MAX_TOTAL || les_core::sha256_hex(&bytes) != digest {
                return Err(INVALID);
            }
            assets.insert(name.clone(), Asset::new(name, bytes, digest)?);
        }
        let receipt_hash = les_core::sha256_hex(&receipt);
        assets.insert(RECEIPT.into(), Asset::new(RECEIPT, receipt, &receipt_hash)?);
        Ok(Self { assets })
    }

    pub fn serve(&self, request: &Request) -> Option<Response> {
        let path = request.uri().path();
        let raw = if path == "/app" {
            ""
        } else {
            path.strip_prefix("/app/")?
        };
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            let mut response = response(StatusCode::METHOD_NOT_ALLOWED, Body::empty());
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            return Some(response);
        }
        if path == "/app" {
            let mut response = response(StatusCode::TEMPORARY_REDIRECT, Body::empty());
            let query = request
                .uri()
                .query()
                .map(|query| format!("?{query}"))
                .unwrap_or_default();
            if let Ok(location) = format!("/app/{query}").parse() {
                response.headers_mut().insert(header::LOCATION, location);
            }
            return Some(response);
        }
        let decoded = percent_encoding::percent_decode_str(raw).decode_utf8().ok();
        let name = decoded
            .as_deref()
            .map(|name| if name.is_empty() { "index.html" } else { name });
        let Some(asset) = name.and_then(|name| self.assets.get(name)) else {
            return Some(response(StatusCode::NOT_FOUND, Body::empty()));
        };
        let unchanged = request
            .headers()
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.split(',').any(|tag| {
                    let tag = tag.trim();
                    tag == "*"
                        || tag.strip_prefix("W/").unwrap_or(tag).as_bytes() == asset.etag.as_bytes()
                })
            });
        let body = if unchanged || request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(asset.bytes.clone())
        };
        let mut response = response(
            if unchanged {
                StatusCode::NOT_MODIFIED
            } else {
                StatusCode::OK
            },
            body,
        );
        let headers = response.headers_mut();
        headers.insert(header::ETAG, asset.etag.clone());
        if !unchanged {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(asset.content_type),
            );
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(asset.bytes.len()));
        }
        Some(response)
    }
}

fn response(status: StatusCode, body: Body) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    for (name, value) in [
        ("content-security-policy", CSP),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
        ("x-les-handler", "rust.frontend"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

pub fn forest(request: &Request) -> Option<Response> {
    let asset = request.uri().path().strip_prefix("/qdrant-visualizer/")?;
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return Some(response(StatusCode::METHOD_NOT_ALLOWED, Body::empty()));
    }
    let bytes: &'static [u8] = match asset {
        "" | "index.html" | "search.html" => {
            let template = if asset == "search.html" {
                include_str!("../../qdrant_visualizer/search.html")
            } else {
                include_str!("../../qdrant_visualizer/index.html")
            };
            let html = template.replacen("<body>", "<body data-native-forest=\"true\">", 1);
            return Some(forest_response(
                request,
                html.into_bytes().into(),
                "text/html; charset=utf-8",
            ));
        }
        "forest.css" => include_bytes!("../../qdrant_visualizer/forest.css"),
        "forest.js" => include_bytes!("../../qdrant_visualizer/forest.js"),
        "search.js" => include_bytes!("../../qdrant_visualizer/search.js"),
        "forest-model.js" => include_bytes!("../../qdrant_visualizer/forest-model.js"),
        "forest-scene.js" => include_bytes!("../../qdrant_visualizer/forest-scene.js"),
        "navigation.js" => include_bytes!("../../qdrant_visualizer/navigation.js"),
        "forest-mist.svg" => include_bytes!("../../qdrant_visualizer/forest-mist.svg"),
        _ => return Some(response(StatusCode::NOT_FOUND, Body::empty())),
    };
    Some(forest_response(
        request,
        Bytes::from_static(bytes),
        mime(asset)?,
    ))
}

fn forest_response(request: &Request, bytes: Bytes, kind: &'static str) -> Response {
    let len = bytes.len();
    let body = if request.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let mut response = response(StatusCode::OK, body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    headers.insert(header::CONTENT_LENGTH, len.into());
    response
}

pub fn classic_link(request: &Request, legacy: Option<&str>) -> Option<Response> {
    let path = request.uri().path();
    if request.method() != Method::GET || !matches!(path, "/classic" | "/les/classic") {
        return None;
    }
    let query = request.uri().query().unwrap_or_default();
    let destination = if path == "/classic" && (query.is_empty() || query == "tab=chat") {
        "/app/".parse().ok()?
    } else if let Some(legacy) = legacy {
        let mut url = reqwest::Url::parse(legacy).ok()?;
        url.set_path(path);
        url.set_query((!query.is_empty()).then_some(query));
        url.as_str().parse().ok()?
    } else {
        match (path, query) {
            ("/classic", "tab=data") => "/app/?panel=data",
            ("/les/classic", "tab=state" | "tab=state&details=1") => "/app/?view=status",
            ("/les/classic", "tab=models") => "/app/?panel=models",
            ("/les/classic", "tab=profiles") => "/app/?panel=profiles",
            ("/les/classic", "tab=tools") => "/app/?panel=mcp",
            _ => return None,
        }
        .parse()
        .ok()?
    };
    let mut result = response(StatusCode::TEMPORARY_REDIRECT, Body::empty());
    result.headers_mut().insert(header::LOCATION, destination);
    Some(result)
}

pub fn legacy_url(raw: &str) -> Result<String, &'static str> {
    const INVALID_URL: &str = "LEGACY_UI_ADDRESS_INVALID";
    let url = reqwest::Url::parse(raw).map_err(|_| INVALID_URL)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/classic"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID_URL);
    }
    Ok(url.to_string())
}

pub async fn configuration(context: &Context, headers: &HeaderMap, url: Option<&str>) -> Response {
    let cors = match auth::authorize(context, headers, false).await {
        Ok(cors) => cors,
        Err(response) => return response,
    };
    let mut configuration = json!({"legacy_url":url});
    if context.public_chat_rust {
        configuration["indexed_attachments"] =
            json!(context.intake.is_some() && context.index_store.is_some());
        configuration["native_model_settings"] = json!(true);
        configuration["native_mail"] = json!(context.upstream.is_none());
    }
    let mut response = json_response(StatusCode::OK, configuration, "rust.frontend");
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().extend(cors);
    response
}
