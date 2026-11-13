use crate::{error, Context};
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use std::sync::Arc;

/// Finish a rejected HTTP/1 request without dropping a small unread incoming
/// body. Otherwise Windows can reset the socket before the response is read.
/// No parsing, forwarding or retry occurs; unbounded/slow bodies close instead.
pub async fn reject(request: Request, mut response: Response) -> Response {
    const LIMIT: usize = 64 * 1024;
    let length = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .map(|v| v.to_str().ok().and_then(|s| s.parse::<usize>().ok()))
        .unwrap_or(Some(0));
    let small = !request.headers().contains_key(header::TRANSFER_ENCODING)
        && length.is_some_and(|n| n <= LIMIT);
    let drained = small
        && matches!(
            tokio::time::timeout(
                std::time::Duration::from_millis(250),
                axum::body::to_bytes(request.into_body(), LIMIT)
            )
            .await,
            Ok(Ok(_))
        );
    if !drained {
        response.headers_mut().insert(
            header::CONNECTION,
            axum::http::HeaderValue::from_static("close"),
        );
    }
    response
}

pub fn local_browser(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(host) = reqwest::Url::parse(&format!("http://{host}")) else {
        return false;
    };
    if !matches!(
        host.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    ) || !host.username().is_empty()
        || host.password().is_some()
    {
        return false;
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let Ok(text) = origin.to_str() else {
            return false;
        };
        let Ok(origin) = reqwest::Url::parse(text) else {
            return false;
        };
        if !matches!(origin.scheme(), "http" | "https")
            || !matches!(
                origin.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
            )
            || !origin.username().is_empty()
            || origin.password().is_some()
            || text.ends_with('/')
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return false;
        }
    }
    true
}

pub fn clean(headers: &HeaderMap, request: bool) -> HeaderMap {
    let mut result = headers.clone();
    let tokens = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .map(str::to_string)
        .collect::<Vec<_>>();
    for name in tokens {
        result.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        result.remove(name);
    }
    if request {
        result.remove(header::HOST);
        result.remove(header::CONTENT_LENGTH);
    }
    result
}

fn upstream_headers(headers: &HeaderMap) -> HeaderMap {
    let mut result = clean(headers, true);
    // The address is fixed separately. Preserve the validated public Host so
    // generated document/preview links keep the browser's same origin.
    if let Some(host) = headers.get(header::HOST) {
        result.insert(header::HOST, host.clone());
    }
    result
}

pub async fn forward(context: Arc<Context>, request: Request) -> Response {
    let Some(origin) = context.upstream.as_ref() else {
        return crate::json_response(
            StatusCode::NOT_IMPLEMENTED,
            serde_json::json!({"code":"NATIVE_ROUTE_NOT_AVAILABLE",
                "detail":"Этот раздел ещё недоступен без прежнего сервера Леса."}),
            "rust.api",
        );
    };
    let (parts, body) = request.into_parts();
    let endpoint = format!(
        "{}{}",
        origin.as_str().trim_end_matches('/'),
        parts
            .uri
            .path_and_query()
            .map(|p| p.as_str())
            .unwrap_or("/")
    );
    let upstream = context
        .client
        .request(parts.method, endpoint)
        .headers(upstream_headers(&parts.headers))
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await;
    let Ok(upstream) = upstream else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Служба Леса недоступна. Дождитесь восстановления и повторите действие.",
        );
    };
    let status = upstream.status();
    let mut headers = clean(upstream.headers(), false);
    // Preserve redirects without exposing the internal server as the new origin.
    if let Some(location) = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    {
        if let Ok(url) = reqwest::Url::parse(&location) {
            let public_origin = parts
                .headers
                .get(header::HOST)
                .and_then(|host| host.to_str().ok())
                .and_then(|host| reqwest::Url::parse(&format!("http://{host}")).ok());
            if url.origin() == origin.origin()
                || public_origin.is_some_and(|public| url.origin() == public.origin())
            {
                let mut relative = url.path().to_owned();
                if let Some(query) = url.query() {
                    relative.push('?');
                    relative.push_str(query);
                }
                if let Some(fragment) = url.fragment() {
                    relative.push('#');
                    relative.push_str(fragment);
                }
                if let Ok(value) = relative.parse() {
                    headers.insert(header::LOCATION, value);
                }
            }
        }
    }
    headers.insert("x-les-handler", "legacy.proxy".parse().unwrap());
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn early_rejection_drains_small_bodies_but_bounds_slow_and_large_inputs() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let polled = Arc::new(AtomicBool::new(false));
        let observed = polled.clone();
        let body = Body::from_stream(futures_util::stream::once(async move {
            observed.store(true, Ordering::SeqCst);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{}"))
        }));
        let request = Request::builder()
            .header(header::CONTENT_LENGTH, "2")
            .body(body)
            .unwrap();
        let reply = reject(request, error(StatusCode::NOT_FOUND, "Not Found")).await;
        assert!(polled.load(Ordering::SeqCst));
        assert_eq!(reply.status(), StatusCode::NOT_FOUND);
        assert!(!reply.headers().contains_key(header::CONNECTION));
        let request = Request::builder()
            .header(header::CONTENT_LENGTH, "70000")
            .body(Body::empty())
            .unwrap();
        let reply = reject(request, error(StatusCode::NOT_FOUND, "Not Found")).await;
        assert_eq!(reply.headers()[header::CONNECTION], "close");
        let request = Request::builder()
            .header(header::CONTENT_LENGTH, "2")
            .body(Body::from_stream(futures_util::stream::pending::<
                Result<axum::body::Bytes, std::io::Error>,
            >()))
            .unwrap();
        let start = std::time::Instant::now();
        let reply = reject(request, error(StatusCode::NOT_FOUND, "Not Found")).await;
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(reply.headers()[header::CONNECTION], "close");
    }
    #[test]
    fn foreign_browser_and_hop_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:1234".parse().unwrap());
        assert!(local_browser(&headers));
        headers.insert("origin", "https://evil.invalid".parse().unwrap());
        assert!(!local_browser(&headers));
        headers.insert("origin", "http://127.0.0.1:5678".parse().unwrap());
        assert!(local_browser(&headers));
        headers.insert("connection", "x-internal, keep-alive".parse().unwrap());
        headers.insert("x-internal", "do-not-forward".parse().unwrap());
        assert!(!clean(&headers, true).contains_key("x-internal"));
        assert!(!clean(&headers, true).contains_key("host"));
    }
}
