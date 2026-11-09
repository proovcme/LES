//! Native model HTTP, with pinned DNS and an owned, cancellable system TLS process.
use axum::{body::Bytes, http::StatusCode};
use futures_util::{
    stream::{self, BoxStream},
    StreamExt,
};
use reqwest::Url;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub type Source = BoxStream<'static, Result<Bytes, &'static str>>;
pub struct Reply {
    pub status: StatusCode,
    pub source: Source,
    pub location: Option<String>,
    pub content_type: Option<String>,
    pub mcp_session: Option<String>,
}
#[derive(Default)]
pub struct McpHeaders {
    pub session: Option<String>,
    pub protocol: Option<String>,
    pub terminate: bool,
}
const FAILED: &str = "UPSTREAM_UNREACHABLE";
const LIMIT: &str = "UPSTREAM_RESPONSE_TOO_LARGE";

pub fn source(response: reqwest::Response) -> Source {
    response
        .bytes_stream()
        .map(|v| {
            v.map_err(|e| {
                if e.is_timeout() {
                    "UPSTREAM_TIMEOUT"
                } else {
                    "UPSTREAM_STREAM_INTERRUPTED"
                }
            })
        })
        .boxed()
}
pub async fn open(
    url: Url,
    addresses: Vec<IpAddr>,
    authorization: Option<String>,
    data: Vec<u8>,
    timeout: Duration,
    system_tls: bool,
) -> Result<Reply, &'static str> {
    open_request(
        url,
        addresses,
        authorization,
        data,
        timeout,
        system_tls,
        false,
        None,
    )
    .await
}
pub async fn get(
    url: Url,
    addresses: Vec<IpAddr>,
    timeout: Duration,
) -> Result<Reply, &'static str> {
    open_request(url, addresses, None, Vec::new(), timeout, false, true, None).await
}
pub async fn get_authorized(
    url: Url,
    addresses: Vec<IpAddr>,
    authorization: Option<String>,
    timeout: Duration,
) -> Result<Reply, &'static str> {
    open_request(
        url,
        addresses,
        authorization,
        Vec::new(),
        timeout,
        false,
        true,
        None,
    )
    .await
}
pub async fn mcp(
    url: Url,
    addresses: Vec<IpAddr>,
    data: Vec<u8>,
    timeout: Duration,
    headers: McpHeaders,
) -> Result<Reply, &'static str> {
    for value in [&headers.session, &headers.protocol].into_iter().flatten() {
        if value.is_empty()
            || value.len() > 2048
            || !value.bytes().all(|v| (0x21..=0x7e).contains(&v))
        {
            return Err("MCP_SESSION_INVALID");
        }
    }
    open_request(
        url,
        addresses,
        None,
        data,
        timeout,
        false,
        false,
        Some(headers),
    )
    .await
}
async fn open_request(
    url: Url,
    addresses: Vec<IpAddr>,
    authorization: Option<String>,
    data: Vec<u8>,
    timeout: Duration,
    system_tls: bool,
    get: bool,
    mcp: Option<McpHeaders>,
) -> Result<Reply, &'static str> {
    if addresses.is_empty() || addresses.len() > 64 || data.len() > 4 * 1024 * 1024 {
        return Err("MODEL_BRIDGE_REQUEST_INVALID");
    }
    if authorization.as_ref().is_some_and(|v| {
        v.len() > 4096 || !v.starts_with("Bearer ") || v.chars().any(char::is_control)
    }) {
        return Err("MODEL_BRIDGE_REQUEST_INVALID");
    }
    if system_tls || url.scheme() == "https" {
        return system(url, addresses, authorization, data, timeout, get, mcp).await;
    }
    let host = url.host_str().ok_or("ENDPOINT_HOST_REQUIRED")?;
    let port = url.port_or_known_default().ok_or("ENDPOINT_URL_INVALID")?;
    let sockets = addresses
        .iter()
        .map(|ip| SocketAddr::new(*ip, port))
        .collect::<Vec<_>>();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(5))
        .resolve_to_addrs(host, &sockets)
        .build()
        .map_err(|_| FAILED)?;
    let mut request = client
        .request(
            if mcp.as_ref().is_some_and(|v| v.terminate) {
                reqwest::Method::DELETE
            } else if get {
                reqwest::Method::GET
            } else {
                reqwest::Method::POST
            },
            url,
        )
        .header("content-type", "application/json")
        .header("user-agent", "LES-Agent/1.0")
        .header(
            "accept",
            if mcp.is_some() {
                "application/json, text/event-stream"
            } else if get {
                "text/html, application/json, text/plain"
            } else {
                "application/json"
            },
        )
        .header("accept-encoding", "identity")
        .body(data)
        .timeout(timeout);
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    if let Some(mcp) = mcp {
        if let Some(session) = mcp.session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(protocol) = mcp.protocol {
            request = request.header("mcp-protocol-version", protocol);
        }
    }
    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "UPSTREAM_TIMEOUT"
        } else {
            FAILED
        }
    })?;
    if !response
        .remote_addr()
        .is_some_and(|v| addresses.contains(&v.ip()))
    {
        return Err("CONNECTED_PEER_MISMATCH");
    }
    if !get && response.status().is_redirection() {
        return Err("UPSTREAM_REDIRECT_REJECTED");
    }
    if response.status().is_success()
        && response
            .headers()
            .get_all("content-encoding")
            .iter()
            .any(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return Err("UPSTREAM_RESPONSE_INVALID");
    }
    Ok(Reply {
        mcp_session: response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        status: response.status(),
        location: response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        content_type: response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        source: source(response),
    })
}

struct Receiving {
    receiver: tokio::sync::mpsc::Receiver<Result<Bytes, &'static str>>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Receiving {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
struct BeforeReady(Option<Arc<AtomicBool>>);
impl Drop for BeforeReady {
    fn drop(&mut self) {
        if let Some(cancel) = &self.0 {
            cancel.store(true, Ordering::Release);
        }
    }
}
struct Metadata {
    status: StatusCode,
    location: Option<String>,
    content_type: Option<String>,
    mcp_session: Option<String>,
}
fn headers(input: &mut impl BufRead, get: bool) -> Result<Metadata, &'static str> {
    let mut consumed = 0usize;
    loop {
        let mut status = None;
        let mut encoding_ok = true;
        let mut location = None;
        let mut content_type = None;
        let mut mcp_session = None;
        loop {
            let mut line = Vec::new();
            let size = (&mut *input)
                .take(16385)
                .read_until(b'\n', &mut line)
                .map_err(|_| FAILED)?;
            consumed = consumed.saturating_add(size);
            if size == 0 {
                return Err(FAILED);
            }
            if line.len() > 16384 || consumed > 65536 {
                return Err(LIMIT);
            }
            let line = std::str::from_utf8(&line)
                .map_err(|_| "UPSTREAM_RESPONSE_INVALID")?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if line.starts_with("HTTP/") {
                let raw = line
                    .split_whitespace()
                    .nth(1)
                    .ok_or("UPSTREAM_RESPONSE_INVALID")?;
                status = Some(
                    StatusCode::from_bytes(raw.as_bytes())
                        .map_err(|_| "UPSTREAM_RESPONSE_INVALID")?,
                );
            } else if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("location") {
                    location = Some(value.trim().to_string());
                }
                if key.eq_ignore_ascii_case("content-type") {
                    content_type = Some(value.trim().to_string());
                }
                if key.eq_ignore_ascii_case("mcp-session-id") {
                    mcp_session = Some(value.trim().to_string());
                }
                if key.eq_ignore_ascii_case("content-encoding")
                    && !value.trim().eq_ignore_ascii_case("identity")
                {
                    encoding_ok = false;
                }
            }
        }
        let status = status.ok_or("UPSTREAM_RESPONSE_INVALID")?;
        if status.is_informational() {
            continue;
        }
        if !get && status.is_redirection() {
            return Err("UPSTREAM_REDIRECT_REJECTED");
        }
        if status.is_success() && !encoding_ok {
            return Err("UPSTREAM_RESPONSE_INVALID");
        }
        return Ok(Metadata {
            status,
            location,
            content_type,
            mcp_session,
        });
    }
}

async fn system(
    url: Url,
    addresses: Vec<IpAddr>,
    authorization: Option<String>,
    data: Vec<u8>,
    timeout: Duration,
    get: bool,
    mcp: Option<McpHeaders>,
) -> Result<Reply, &'static str> {
    let mut config = String::new();
    if let Some(mcp) = &mcp {
        if let Some(session) = &mcp.session {
            crate::external_embed::option(
                &mut config,
                "header",
                &format!("Mcp-Session-Id: {session}"),
            )?;
        }
        if let Some(protocol) = &mcp.protocol {
            crate::external_embed::option(
                &mut config,
                "header",
                &format!("MCP-Protocol-Version: {protocol}"),
            )?;
        }
    }
    if mcp.as_ref().is_some_and(|v| v.terminate) {
        crate::external_embed::option(&mut config, "request", "DELETE")?;
    } else if !get {
        crate::external_embed::option(
            &mut config,
            "data-binary",
            std::str::from_utf8(&data).map_err(|_| "MODEL_BRIDGE_REQUEST_INVALID")?,
        )?;
    }
    if let Some(secret) = authorization {
        crate::external_embed::option(&mut config, "header", &format!("Authorization: {secret}"))?;
    }
    let mut command =
        Command::new(crate::external_embed::executable().ok_or("UPSTREAM_TLS_UNAVAILABLE")?);
    command
        .args([
            "--disable",
            "--silent",
            "--globoff",
            "--no-buffer",
            "--noproxy",
            "*",
            "--proto",
            "=http,https",
            "--connect-timeout",
            "5",
            "--max-time",
        ])
        .arg(timeout.as_secs_f64().to_string())
        .args([
            "--max-filesize",
            "67108864",
            "--dump-header",
            "-",
            "--header",
            "Content-Type: application/json",
            "--header",
            if mcp.is_some() {
                "Accept: application/json, text/event-stream"
            } else if get {
                "Accept: text/html, application/json, text/plain"
            } else {
                "Accept: application/json"
            },
            "--user-agent",
            "LES-Agent/1.0",
            "--header",
            "Accept-Encoding: identity",
            "--header",
            "Expect:",
            "--config",
            "-",
            "--url",
        ])
        .arg(url.as_str());
    let host = url.host_str().ok_or("ENDPOINT_HOST_REQUIRED")?;
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_err() {
        let peers = addresses
            .iter()
            .map(|v| match v {
                IpAddr::V4(v) => v.to_string(),
                IpAddr::V6(v) => format!("[{v}]"),
            })
            .collect::<Vec<_>>()
            .join(",");
        command.arg("--resolve").arg(format!(
            "{host}:{}:{peers}",
            url.port_or_known_default().ok_or("ENDPOINT_URL_INVALID")?
        ));
    }
    les_core::prepare_child(&mut command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| FAILED)?;
    let guard = match les_core::JobGuard::own(&child, None) {
        Ok(v) => v,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("MODEL_PROCESS_OWNERSHIP_FAILED");
        }
    };
    let mut input = child.stdin.take().ok_or(FAILED)?;
    let output = child.stdout.take().ok_or(FAILED)?;
    let child = Arc::new(Mutex::new(child));
    let cancel = Arc::new(AtomicBool::new(false));
    let exit = Arc::new(AtomicI32::new(-1));
    let mut before_ready = BeforeReady(Some(cancel.clone()));
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
    let watch_cancel = cancel.clone();
    let watch_exit = exit.clone();
    let watch_child = child.clone();
    let deadline = Instant::now() + timeout + Duration::from_secs(2);
    let watching = thread::Builder::new()
        .name("les-model-tls-owner".into())
        .spawn(move || {
            let _guard = guard;
            loop {
                let Ok(mut child) = watch_child.lock() else {
                    watch_exit.store(-2, Ordering::Release);
                    return;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        watch_exit.store(status.code().unwrap_or(-2), Ordering::Release);
                        return;
                    }
                    Ok(None)
                        if !watch_cancel.load(Ordering::Acquire) && Instant::now() < deadline => {}
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        watch_exit.store(
                            if Instant::now() >= deadline { -3 } else { -2 },
                            Ordering::Release,
                        );
                        return;
                    }
                }
                drop(child);
                thread::sleep(Duration::from_millis(20));
            }
        });
    if watching.is_err() {
        if let Ok(mut child) = child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        return Err(FAILED);
    }
    let writing_cancel = cancel.clone();
    if thread::Builder::new()
        .name("les-model-tls-input".into())
        .spawn(move || {
            if input.write_all(config.as_bytes()).is_err() {
                writing_cancel.store(true, Ordering::Release);
            }
        })
        .is_err()
    {
        cancel.store(true, Ordering::Release);
        return Err(FAILED);
    }
    let read_cancel = cancel.clone();
    let read_exit = exit.clone();
    if thread::Builder::new()
        .name("les-model-tls-output".into())
        .spawn(move || {
            let mut reader = BufReader::with_capacity(8192, output);
            let status = match headers(&mut reader, get) {
                Ok(status) => status,
                Err(code) => {
                    let _ = ready_sender.send(Err(code));
                    read_cancel.store(true, Ordering::Release);
                    return;
                }
            };
            if ready_sender.send(Ok(status)).is_err() {
                read_cancel.store(true, Ordering::Release);
                return;
            }
            let mut consumed = 0usize;
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => {
                        consumed = consumed.saturating_add(size);
                        if consumed > 64 * 1024 * 1024 {
                            let _ = sender.blocking_send(Err(LIMIT));
                            read_cancel.store(true, Ordering::Release);
                            return;
                        }
                        if sender
                            .blocking_send(Ok(Bytes::copy_from_slice(&buffer[..size])))
                            .is_err()
                        {
                            read_cancel.store(true, Ordering::Release);
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = sender.blocking_send(Err("UPSTREAM_STREAM_INTERRUPTED"));
                        read_cancel.store(true, Ordering::Release);
                        return;
                    }
                }
            }
            while read_exit.load(Ordering::Acquire) == -1 && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            let code = read_exit.load(Ordering::Acquire);
            if code != 0 {
                let error = match code {
                    28 | -3 => "UPSTREAM_TIMEOUT",
                    60 => "UPSTREAM_TLS_FAILED",
                    63 => LIMIT,
                    _ => "UPSTREAM_STREAM_INTERRUPTED",
                };
                let _ = sender.blocking_send(Err(error));
            }
        })
        .is_err()
    {
        cancel.store(true, Ordering::Release);
        return Err(FAILED);
    }
    let status = match tokio::time::timeout(timeout + Duration::from_secs(2), ready_receiver).await
    {
        Ok(Ok(Ok(status))) => status,
        Ok(Ok(Err(code))) => {
            cancel.store(true, Ordering::Release);
            return Err(code);
        }
        _ => {
            cancel.store(true, Ordering::Release);
            return Err("UPSTREAM_TIMEOUT");
        }
    };
    let receiving = Receiving { receiver, cancel };
    before_ready.0 = None;
    let source = stream::unfold(receiving, |mut state| async move {
        let part = state.receiver.recv().await?;
        Some((part, state))
    })
    .boxed();
    Ok(Reply {
        status: status.status,
        source,
        location: status.location,
        content_type: status.content_type,
        mcp_session: status.mcp_session,
    })
}
