use crate::{error, json_response, Context};
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

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
    let (parts, body) = request.into_parts();
    let endpoint = format!(
        "{}{}",
        context.upstream,
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
            if let Ok(origin) = reqwest::Url::parse(&context.upstream) {
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
    }
    headers.insert("x-les-handler", "legacy.proxy".parse().unwrap());
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

pub async fn authorize(context: &Context, headers: &HeaderMap) -> Result<HeaderMap, Response> {
    authorize_role(context, headers, false).await
}

pub async fn authorize_role(
    context: &Context,
    headers: &HeaderMap,
    admin: bool,
) -> Result<HeaderMap, Response> {
    let check = async {
        let mut response = context
            .client
            .get(format!(
                "{}/api/light/{}",
                context.upstream,
                if admin {
                    "native-admin-authorization"
                } else {
                    "native-authorization"
                }
            ))
            .headers(upstream_headers(headers))
            .send()
            .await
            .map_err(|_| ())?;
        let status = response.status();
        let mut cors = HeaderMap::new();
        for (name, value) in response.headers() {
            if name.as_str().starts_with("access-control-") || name == header::VARY {
                cors.insert(name.clone(), value.clone());
            }
        }
        let mut bytes = Vec::new();
        while let Some(block) = response.chunk().await.map_err(|_| ())? {
            if bytes.len() + block.len() > 16384 {
                return Err(());
            }
            bytes.extend_from_slice(&block);
        }
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| ())?;
        Ok((status, value, cors))
    };
    match tokio::time::timeout(Duration::from_secs(15), check).await {
        Ok(Ok((status, value, cors))) if status == StatusCode::OK && value["allowed"] == true => {
            Ok(cors)
        }
        Ok(Ok((status, value, cors)))
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN =>
        {
            let mut response = json_response(status, value, "rust.workspace");
            response.headers_mut().extend(cors);
            Err(response)
        }
        _ => Err(json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"detail":"Не удалось проверить доступ к диалогу. Дождитесь восстановления Леса и повторите действие."}),
            "rust.workspace",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
