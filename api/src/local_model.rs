//! Private HTTP bridge for already-resolved loopback model calls.
use crate::{json_response, ollama_chat, ollama_embed, Context};
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, Method, StatusCode},
    response::Response,
};
use reqwest::Url;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

pub const PATH: &str = "/api/light/internal-model-http";
pub const TOKEN_ENV: &str = "LES_INTERNAL_MODEL_TOKEN";
const TOKEN_HEADER: &str = "x-les-model-token";
const TARGET_HEADER: &str = "x-les-model-target";
const ERROR_HEADER: &str = "x-les-model-bridge-error";

enum Reply {
    Raw,
    Chat(ollama_chat::Limits),
    Embed(ollama_embed::Limits),
}

impl Reply {
    fn parse(path: &str, headers: &HeaderMap) -> Option<Self> {
        let chat = headers.get(ollama_chat::LIMITS_HEADER);
        let embed = headers.get(ollama_embed::LIMITS_HEADER);
        match (path, chat, embed) {
            (_, None, None) => Some(Self::Raw),
            ("/api/chat", Some(value), None) => {
                ollama_chat::Limits::parse(value.to_str().ok()?).map(Self::Chat)
            }
            ("/api/embed", None, Some(value)) => {
                ollama_embed::Limits::parse(value.to_str().ok()?).map(Self::Embed)
            }
            _ => None,
        }
    }
}

pub struct Bridge {
    token: String,
    listener_port: u16,
    upstream_port: u16,
    client: reqwest::Client,
}

impl Bridge {
    pub fn new(
        token: String,
        listener_port: u16,
        upstream_port: u16,
    ) -> Result<Self, &'static str> {
        if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("Invalid internal model HTTP token");
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::from((Ipv6Addr::LOCALHOST, 0)),
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                ],
            )
            .build()
            .map_err(|_| "Internal model HTTP client unavailable")?;
        Ok(Self {
            token,
            listener_port,
            upstream_port,
            client,
        })
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(token) = headers.get(TOKEN_HEADER) else {
            return false;
        };
        !headers.contains_key(header::ORIGIN)
            && token.as_bytes().len() == self.token.len()
            && self
                .token
                .bytes()
                .zip(token.as_bytes())
                .fold(0, |difference, (expected, actual)| {
                    difference | (expected ^ actual)
                })
                == 0
    }

    fn target(&self, headers: &HeaderMap) -> Option<Url> {
        let raw = headers.get(TARGET_HEADER)?.to_str().ok()?;
        let url = Url::parse(raw).ok()?;
        // Reject alternate URL spellings (including credentials and ambiguous
        // encodings) before giving the request to the network client.
        if url.as_str() != raw
            || url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "/api/chat" | "/api/embed")
            || [self.listener_port, self.upstream_port].contains(&url.port_or_known_default()?)
        {
            return None;
        }
        let host = url.host_str()?;
        if host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        {
            Some(url)
        } else {
            None
        }
    }

    async fn send(&self, request: Request) -> Response {
        let Some(target) = self.target(request.headers()) else {
            return failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_TARGET_REJECTED");
        };
        let Some(reply) = Reply::parse(target.path(), request.headers()) else {
            return failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_REQUEST_INVALID");
        };
        let (parts, body) = request.into_parts();
        let Some(length) = parts.headers.get(header::CONTENT_LENGTH) else {
            return failure(StatusCode::LENGTH_REQUIRED, "MODEL_BRIDGE_REQUEST_INVALID");
        };
        if parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some("application/json")
        {
            return failure(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "MODEL_BRIDGE_REQUEST_INVALID",
            );
        }
        let mut outgoing = self
            .client
            .post(target.clone())
            .header(header::CONTENT_LENGTH, length)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(reqwest::Body::wrap_stream(body.into_data_stream()));
        if !matches!(reply, Reply::Raw) {
            outgoing = outgoing.header(header::ACCEPT_ENCODING, "identity");
        }
        let upstream = match outgoing.send().await {
            Ok(response) => response,
            Err(error) if error.is_timeout() => {
                return failure(StatusCode::GATEWAY_TIMEOUT, "UPSTREAM_TIMEOUT")
            }
            Err(_) => return failure(StatusCode::BAD_GATEWAY, "UPSTREAM_UNREACHABLE"),
        };
        if !upstream.remote_addr().is_some_and(|peer| {
            let ip = peer.ip();
            match target.host_str() {
                Some("localhost") => {
                    ip == IpAddr::from(Ipv4Addr::LOCALHOST)
                        || ip == IpAddr::from(Ipv6Addr::LOCALHOST)
                }
                Some(host) => host.trim_matches(['[', ']']).parse::<IpAddr>() == Ok(ip),
                None => false,
            }
        }) {
            return failure(StatusCode::BAD_GATEWAY, "CONNECTED_PEER_MISMATCH");
        }
        if upstream.status().is_redirection() {
            return failure(StatusCode::BAD_GATEWAY, "UPSTREAM_REDIRECT_REJECTED");
        }
        let mut response = Response::builder()
            .status(upstream.status())
            .header("x-les-handler", "rust.model-http")
            .header("cache-control", "no-store");
        if upstream.status().is_success() && !matches!(reply, Reply::Raw) {
            if upstream
                .headers()
                .get_all(header::CONTENT_ENCODING)
                .iter()
                .any(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
            {
                return failure(StatusCode::BAD_GATEWAY, "UPSTREAM_RESPONSE_INVALID");
            }
            let (content_type, protocol, body) = match reply {
                Reply::Chat(limits) => (
                    "application/x-ndjson",
                    ollama_chat::PROTOCOL,
                    ollama_chat::body(upstream, limits),
                ),
                Reply::Embed(limits) => match ollama_embed::receive(upstream, limits).await {
                    Ok(vectors) => (
                        "application/octet-stream",
                        ollama_embed::PROTOCOL,
                        Body::from(vectors),
                    ),
                    Err(code) => return failure(StatusCode::BAD_GATEWAY, code),
                },
                Reply::Raw => unreachable!(),
            };
            return response
                .header(header::CONTENT_TYPE, content_type)
                .header(ollama_chat::PROTOCOL_HEADER, protocol)
                .body(body)
                .expect("valid private model response headers");
        }
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_ENCODING,
            header::CONTENT_LENGTH,
        ] {
            if let Some(value) = upstream.headers().get(&name) {
                response = response.header(name, value);
            }
        }
        response
            .body(Body::from_stream(upstream.bytes_stream()))
            .expect("validated upstream status and headers")
    }
}

fn failure(status: StatusCode, code: &'static str) -> Response {
    eprintln!(
        "{}",
        serde_json::json!({"event":"model_http_failed","code":code,"status":status.as_u16()})
    );
    let mut response = json_response(
        status,
        serde_json::json!({"detail":code}),
        "rust.model-http",
    );
    response
        .headers_mut()
        .insert(ERROR_HEADER, header::HeaderValue::from_static(code));
    response
}

pub async fn handle(context: &Context, request: Request) -> Response {
    if let Some(bridge) = &context.local_model {
        if request.method() == Method::POST && bridge.authorized(request.headers()) {
            return bridge.send(request).await;
        }
    }
    let mut response = json_response(
        StatusCode::NOT_FOUND,
        serde_json::json!({"detail":"Not Found"}),
        "rust.model-http",
    );
    response.headers_mut().insert(
        ERROR_HEADER,
        header::HeaderValue::from_static("UPSTREAM_UNREACHABLE"),
    );
    response
}
