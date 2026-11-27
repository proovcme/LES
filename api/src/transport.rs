use axum::{
    extract::Request,
    http::{header, HeaderMap},
    response::Response,
};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error;
    use axum::{body::Body, http::StatusCode};
    use std::sync::Arc;
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
