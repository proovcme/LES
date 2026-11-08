//! Private HTTP bridge for already-resolved loopback model calls.
use crate::{external_embed, json_response, ollama_chat, ollama_embed, openai_chat, Context};
use axum::{
    body::{to_bytes, Body},
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
    OpenAiChat(ollama_chat::Limits, bool),
    Embed(ollama_embed::Limits, ollama_embed::Format),
}

impl Reply {
    fn parse(path: &str, headers: &HeaderMap) -> Option<Self> {
        let wire = headers.get(openai_chat::WIRE);
        if path.ends_with("/chat/completions") {
            if headers.contains_key(ollama_embed::LIMITS_HEADER) {
                return None;
            }
            let limits = ollama_chat::Limits::parse(
                headers.get(ollama_chat::LIMITS_HEADER)?.to_str().ok()?,
            )?;
            let streaming = match wire?.to_str().ok()? {
                "sse" => true,
                "json" => false,
                _ => return None,
            };
            return Some(Self::OpenAiChat(limits, streaming));
        }
        if wire.is_some() {
            return None;
        }
        let chat = headers.get(ollama_chat::LIMITS_HEADER);
        let embed = headers.get(ollama_embed::LIMITS_HEADER);
        match (path, chat, embed) {
            ("/api/chat" | "/api/embed", None, None) => Some(Self::Raw),
            ("/api/chat", Some(value), None) => {
                ollama_chat::Limits::parse(value.to_str().ok()?).map(Self::Chat)
            }
            ("/api/embed", None, Some(value)) => ollama_embed::Limits::parse(value.to_str().ok()?)
                .map(|limits| Self::Embed(limits, ollama_embed::Format::Ollama)),
            (path, None, Some(value)) if path.ends_with("/embeddings") => {
                ollama_embed::Limits::parse(value.to_str().ok()?)
                    .map(|limits| Self::Embed(limits, ollama_embed::Format::OpenAi))
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

    pub(crate) fn authorized(&self, headers: &HeaderMap) -> bool {
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
            || !(matches!(url.path(), "/api/chat" | "/api/embed")
                || url.path().ends_with("/embeddings")
                || url.path().ends_with("/chat/completions"))
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
        if request.headers().contains_key("x-les-chat-addresses") {
            return self.send_external_chat(request).await;
        }
        if request.headers().contains_key(external_embed::ADDRESSES) {
            return self.send_external_embedding(request).await;
        }
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
        let data = if let Reply::OpenAiChat(_, streaming) = &reply {
            let Ok(bytes) = to_bytes(body, 4 * 1024 * 1024).await else {
                return failure(StatusCode::PAYLOAD_TOO_LARGE, "UPSTREAM_REQUEST_TOO_LARGE");
            };
            let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_REQUEST_INVALID");
            };
            if !json.is_object() || json["stream"].as_bool().unwrap_or(false) != *streaming {
                return failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_REQUEST_INVALID");
            }
            reqwest::Body::from(bytes)
        } else {
            reqwest::Body::wrap_stream(body.into_data_stream())
        };
        let mut outgoing = self
            .client
            .post(target.clone())
            .header(header::CONTENT_LENGTH, length)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(data);
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
                Reply::OpenAiChat(limits, streaming) => (
                    "application/x-ndjson",
                    openai_chat::PROTOCOL,
                    openai_chat::body(upstream, limits, streaming),
                ),
                Reply::Embed(limits, format) => {
                    match ollama_embed::receive(upstream, limits, format).await {
                        Ok(vectors) => (
                            "application/octet-stream",
                            ollama_embed::PROTOCOL,
                            Body::from(vectors),
                        ),
                        Err(code) => return failure(StatusCode::BAD_GATEWAY, code),
                    }
                }
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

    async fn send_external_chat(&self, request: Request) -> Response {
        use futures_util::{stream, StreamExt};
        let headers = request.headers().clone();
        let get = |key: &str| headers.get(key).and_then(|v| v.to_str().ok());
        let invalid = || failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_TARGET_REJECTED");
        let Some(raw) = get(TARGET_HEADER) else {
            return invalid();
        };
        let Ok(target) = Url::parse(raw) else {
            return invalid();
        };
        if target.as_str() != raw
            || !matches!(target.scheme(), "http" | "https")
            || !target.username().is_empty()
            || target.password().is_some()
            || target.query().is_some()
            || target.fragment().is_some()
            || !(target.path().ends_with("/chat/completions")
                || target.path().ends_with("/api/chat"))
        {
            return invalid();
        }
        let Some(locality) = get("x-les-chat-locality") else {
            return invalid();
        };
        if locality == "remote" && target.scheme() != "https" {
            return invalid();
        }
        let Some(port) = target.port_or_known_default() else {
            return invalid();
        };
        if locality == "loopback" && [self.listener_port, self.upstream_port].contains(&port) {
            return invalid();
        }
        let Some(raw_addresses) = get("x-les-chat-addresses") else {
            return invalid();
        };
        let Ok(addresses) = raw_addresses
            .split(',')
            .take(65)
            .map(str::parse::<IpAddr>)
            .collect::<Result<Vec<_>, _>>()
        else {
            return invalid();
        };
        if addresses.is_empty()
            || addresses.len() > 64
            || addresses
                .iter()
                .any(|ip| crate::model_runtime::permitted(*ip, locality).is_err())
        {
            return invalid();
        }
        if let Some(host) = target.host_str() {
            if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
                if addresses.as_slice() != [ip] {
                    return invalid();
                }
            }
        }
        let Some(seconds) = get("x-les-chat-deadline")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| (1..=600).contains(v))
        else {
            return invalid();
        };
        let path = if target.path().ends_with("/api/chat") {
            "/api/chat"
        } else {
            target.path()
        };
        let Some(reply) = Reply::parse(path, request.headers()) else {
            return invalid();
        };
        let (limits, streaming, native) = match reply {
            Reply::Chat(limits) => (limits, true, true),
            Reply::OpenAiChat(limits, streaming) => (limits, streaming, false),
            _ => return invalid(),
        };
        let authorization = match request.headers().get("authorization") {
            Some(value) => match value.to_str() {
                Ok(v) => Some(v.to_string()),
                Err(_) => return invalid(),
            },
            None => None,
        };
        let Ok(bytes) = to_bytes(request.into_body(), 4 * 1024 * 1024).await else {
            return failure(StatusCode::PAYLOAD_TOO_LARGE, "UPSTREAM_REQUEST_TOO_LARGE");
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_REQUEST_INVALID");
        };
        if !value.is_object() || !native && value["stream"].as_bool().unwrap_or(false) != streaming
        {
            return failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_REQUEST_INVALID");
        }
        // Open inside the response stream, so closing it cancels a pending TLS handshake too.
        let body=Body::from_stream(stream::once(async move {
            let body=match crate::model_http::open(target,addresses,authorization,bytes.to_vec(),Duration::from_secs(seconds),true).await {
                Ok(reply) if reply.status.is_success()=>{
                    if native{ollama_chat::from_source(reply.source,limits)}else{openai_chat::from_source(reply.source,limits,streaming)}
                }
                Ok(reply)=>Body::from(format!("{}\n",serde_json::json!({"error":format!("UPSTREAM_HTTP_ERROR: {}",reply.status.as_u16())}))),
                Err(code)=>Body::from(format!("{}\n",serde_json::json!({"error":code}))),
            };
            body.into_data_stream()
        }).flatten());
        Response::builder()
            .status(StatusCode::OK)
            .header("x-les-handler", "rust.model-http")
            .header("cache-control", "no-store")
            .header("content-type", "application/x-ndjson")
            .header(
                ollama_chat::PROTOCOL_HEADER,
                if native {
                    ollama_chat::PROTOCOL
                } else {
                    openai_chat::PROTOCOL
                },
            )
            .body(body)
            .expect("valid private native chat response")
    }

    async fn send_external_embedding(&self, request: Request) -> Response {
        let invalid = || failure(StatusCode::BAD_REQUEST, "MODEL_BRIDGE_TARGET_REJECTED");
        let Some(target) = request
            .headers()
            .get(TARGET_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Url::parse(value).ok().filter(|url| url.as_str() == value))
        else {
            return invalid();
        };
        let Ok(call) = external_embed::Call::new(
            target,
            request.headers(),
            [self.listener_port, self.upstream_port],
        ) else {
            return invalid();
        };
        if request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            != Some("application/json")
        {
            return invalid();
        }
        let Ok(data) = to_bytes(request.into_body(), 4 * 1024 * 1024).await else {
            return failure(StatusCode::PAYLOAD_TOO_LARGE, "UPSTREAM_REQUEST_TOO_LARGE");
        };
        match tokio::task::spawn_blocking(move || call.send(&data)).await {
            Ok(Ok((status, bytes))) => Response::builder()
                .status(status)
                .header("x-les-handler", "rust.model-http")
                .header("cache-control", "no-store")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(ollama_chat::PROTOCOL_HEADER, ollama_embed::PROTOCOL)
                .body(Body::from(bytes))
                .expect("valid private embedding response"),
            Ok(Err(code)) => failure(StatusCode::BAD_GATEWAY, code),
            Err(_) => failure(StatusCode::BAD_GATEWAY, "UPSTREAM_UNREACHABLE"),
        }
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
