mod agent_state;
mod auth;
mod auth_keys;
mod catalog;
mod frontend;
mod index_store;
mod intake;
mod library;
mod local_model;
mod ollama_chat;
mod ollama_embed;
mod pdf_text;
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
    pub auth_policy: tokio::sync::OnceCell<auth::Policy>,
    pub frontend: Option<frontend::Frontend>,
    pub legacy_ui_url: Option<String>,
    pub local_model: Option<local_model::Bridge>,
    pub intake: Option<intake::Service>,
    pub index_store: Option<index_store::Store>,
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
    if request.uri().path() == auth::POLICY_PATH || intake::internal(request.uri().path()) {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    if request.uri().path() == local_model::PATH {
        return local_model::handle(&context, request).await;
    }
    if request.uri().path() == index_store::PATH {
        return index_store::handle(&context, request).await;
    }
    if let Some(frontend) = &context.frontend {
        if let Some(response) = frontend
            .serve(&request)
            .or_else(|| self::frontend::forest(&request))
            .or_else(|| self::frontend::classic_link(&request, context.legacy_ui_url.as_deref()))
        {
            return response;
        }
    }
    if request.uri().path() == "/api/light/frontend" && request.method() == axum::http::Method::GET
    {
        if let Some(url) = &context.legacy_ui_url {
            return frontend::configuration(&context, request.headers(), url).await;
        }
    }
    if let Some(route) = auth_keys::route(request.method(), request.uri().path()) {
        return auth_keys::handle(context, request, route).await;
    }
    if library::handles(request.method().as_str(), request.uri().path()) {
        return library::handle(context, request).await;
    }
    if request.method() == "POST" && request.uri().path() == intake::PATH {
        if context.intake.is_some() {
            return intake::handle(context, request).await;
        }
    }
    if let Some(route) = workspace::route(request.method(), request.uri().path()) {
        return workspace::handle(context, request, route).await;
    }
    if let Some(route) = catalog::route(request.method(), request.uri().path()) {
        return catalog::handle(context, request, route).await;
    }
    if matches!(
        request.uri().path(),
        "/api/workspace/agent-state" | "/api/light/agent-workspace"
    ) && matches!(request.method().as_str(), "GET" | "POST")
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
            "--listen"
                | "--upstream"
                | "--database"
                | "--state"
                | "--frontend-assets"
                | "--legacy-ui-url"
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
    let frontend = values
        .get("--frontend-assets")
        .map(|path| frontend::Frontend::load(std::path::Path::new(path)))
        .transpose()?;
    let legacy_ui_url = values
        .get("--legacy-ui-url")
        .map(|url| frontend::legacy_url(url))
        .transpose()?;
    let token = std::env::var(local_model::TOKEN_ENV).ok();
    let local_model = token
        .as_ref()
        .map(|token| local_model::Bridge::new(token.clone(), listen.port(), upstream.port().unwrap()))
        .transpose()?;
    let ingest_token = std::env::var("LES_INTERNAL_INGEST_TOKEN").ok();
    if ingest_token.as_ref().is_some_and(|token| {
        token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err("Invalid internal ingestion token");
    }
    let index_store = ingest_token.clone().map(index_store::Store::from_env).transpose()?.flatten();
    let intake = ingest_token
        .filter(|_| std::env::var("LES_STARTUP_BACKGROUND_MUTATIONS").as_deref() == Ok("true"))
        .map(intake::Service::new);
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
            auth_policy: tokio::sync::OnceCell::new(),
            frontend,
            legacy_ui_url,
            local_model,
            intake,
            index_store,
        },
    ))
}

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let [action, library] = args.as_slice() {
        if action == "--serve-pdf-text" {
            if pdf_text::serve(std::path::Path::new(library)).is_err() {
                eprintln!("PDF_WORKER_STOPPED");
                std::process::exit(1);
            }
            return;
        }
    }
    if let [action, library, document] = args.as_slice() {
        if action == "--extract-pdf-text" {
            let result = pdf_text::extract(std::path::Path::new(library), std::path::Path::new(document));
            println!("{result}");
            return;
        }
    }
    if std::env::args().skip(1).collect::<Vec<_>>() == ["--contract"] {
        println!(
            "{}",
            json!({"schema":"les.native-api.v1","native":["workspace.sessions","workspace.drafts","projects.crud","projects.links","workspace.notes.read_edit_delete","conversation.context","workspace.agent_state","auth.keys","auth.verify"],
            "legacy_proxy_remaining":true,"authentication_owner":"rust.local_policy","streaming_proxy":true,
            "frontend_assets":"rust.verified-manifest","local_model_http":"rust.private-loopback",
            "local_model_stream":"ollama-chat.v1","local_model_embeddings":"ollama-embed.v1"})
        );
        return;
    }
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let (listen, context) = configuration().map_err(|reason| {
            eprintln!(
                "{}",
                json!({"event":"native_api_configuration_rejected","reason":reason})
            );
            std::io::Error::other(reason)
        })?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            let context = Arc::new(context);
            if context.intake.is_some() {
                tokio::spawn(intake::run(context.clone()));
            }
            axum::serve(
                listener,
                Router::new()
                    .fallback(dispatch)
                    .with_state(context),
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
