mod agent_state;
mod catalog;
mod transport;
mod workspace;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Router,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub struct Context {
    pub database: PathBuf,
    pub state_root: PathBuf,
    pub upstream: String,
    pub client: reqwest::Client,
    pub workspace_slots: Arc<tokio::sync::Semaphore>,
}

pub fn json_response(status: StatusCode, value: Value, engine: &str) -> Response {
    let mut response = (
        status,
        [("content-type", "application/json")],
        value.to_string(),
    )
        .into_response();
    response
        .headers_mut()
        .insert("x-les-handler", engine.parse().unwrap());
    response
}
pub fn error(status: StatusCode, detail: &str) -> Response {
    json_response(status, json!({"detail":detail}), "rust.workspace")
}

async fn dispatch(State(context): State<Arc<Context>>, request: Request) -> Response {
    if !transport::local_browser(request.headers()) {
        return error(StatusCode::FORBIDDEN, "Local LES browser access required");
    }
    if let Some(route) = workspace::route(request.method(), request.uri().path()) {
        return workspace::handle(context, request, route).await;
    }
    if let Some(route) = catalog::route(request.method(), request.uri().path()) {
        return catalog::handle(context, request, route).await;
    }
    if request.uri().path() == "/api/workspace/agent-state"
        && matches!(request.method().as_str(), "GET" | "POST")
    {
        return agent_state::handle(context, request).await;
    }
    transport::forward(context, request).await
}

fn configuration() -> Result<(std::net::SocketAddr, Context), &'static str> {
    let mut args = std::env::args().skip(1);
    let mut values = std::collections::HashMap::new();
    while let Some(key) = args.next() {
        if !matches!(
            key.as_str(),
            "--listen" | "--upstream" | "--database" | "--state"
        ) {
            return Err("Invalid native API argument");
        }
        let value = args.next().ok_or("Missing native API argument")?;
        if values.insert(key, value).is_some() {
            return Err("Duplicate native API argument");
        }
    }
    let listen: std::net::SocketAddr = values
        .get("--listen")
        .ok_or("Native API address missing")?
        .parse()
        .map_err(|_| "Invalid native API address")?;
    if listen.ip() != std::net::Ipv4Addr::LOCALHOST || listen.port() == 0 {
        return Err("Native API requires explicit IPv4 loopback port");
    }
    let upstream = reqwest::Url::parse(
        values
            .get("--upstream")
            .ok_or("Legacy API address missing")?,
    )
    .map_err(|_| "Invalid legacy API address")?;
    if upstream.scheme() != "http"
        || upstream.host_str() != Some("127.0.0.1")
        || upstream.port().is_none()
        || upstream.path() != "/"
        || !upstream.username().is_empty()
        || upstream.password().is_some()
        || upstream.query().is_some()
        || upstream.fragment().is_some()
        || upstream.port() == Some(listen.port())
    {
        return Err("Legacy API requires a separate owned loopback address");
    }
    let database = PathBuf::from(
        values
            .get("--database")
            .ok_or("Native workspace database missing")?,
    );
    if !database.is_absolute() {
        return Err("Native workspace requires an absolute database path");
    }
    let state_root = PathBuf::from(values.get("--state").ok_or("Native state root missing")?);
    if !state_root.is_absolute() {
        return Err("Absolute state root required");
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| "Native API client unavailable")?;
    Ok((
        listen,
        Context {
            database,
            state_root,
            upstream: upstream.to_string().trim_end_matches('/').to_owned(),
            client,
            workspace_slots: Arc::new(tokio::sync::Semaphore::new(16)),
        },
    ))
}

fn main() {
    if std::env::args().skip(1).collect::<Vec<_>>() == ["--contract"] {
        println!(
            "{}",
            json!({"schema":"les.native-api.v1","native":["workspace.sessions","workspace.drafts","projects.crud","projects.links","workspace.notes.read_edit_delete","conversation.context","workspace.agent_state"],
            "legacy_proxy_remaining":true,"authentication_owner":"legacy.require_user","streaming_proxy":true})
        );
        return;
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (listen, context) = configuration().map_err(std::io::Error::other)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            axum::serve(
                listener,
                Router::new()
                    .fallback(dispatch)
                    .with_state(Arc::new(context)),
            )
            .await
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!(
            "{}",
            json!({"event":"native_api_stopped","reason":"NATIVE_API_STOPPED",
            "io_kind":error.downcast_ref::<std::io::Error>().map(|error|format!("{:?}",error.kind()))})
        );
        std::process::exit(1);
    }
}
