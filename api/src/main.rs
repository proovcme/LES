mod agent_state;
mod attachments;
mod auth;
mod auth_keys;
mod catalog;
mod chat;
mod conversation;
mod dataset_reader;
mod derived_cache;
mod document_cache;
mod document_dispatch;
mod document_media;
mod document_parser;
mod documents;
#[cfg(feature = "edge")]
mod edge;
mod embedding_inference;
mod embedding_worker;
mod external_embed;
mod frontend;
mod history;
mod index_batch;
mod index_bootstrap;
mod index_store;
mod inference;
mod intake;
mod library;
mod local_model;
mod mail;
mod mcp;
mod model_answer;
mod model_http;
mod model_registry;
mod model_runtime;
mod ollama_chat;
mod ollama_embed;
mod openai_chat;
mod pdf_ocr;
mod pdf_text;
mod profiles;
mod status;
mod transport;
mod web;
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
    pub listen_port: u16,
    pub database: PathBuf,
    pub state_root: PathBuf,
    pub upstream: Option<reqwest::Url>,
    pub client: reqwest::Client,
    pub workspace_slots: Arc<tokio::sync::Semaphore>,
    pub attachment_slots: Arc<tokio::sync::Semaphore>,
    pub upload_slots: Arc<tokio::sync::Semaphore>,
    pub upload_owner: tokio::sync::OnceCell<std::fs::File>,
    pub auth_policy: tokio::sync::OnceCell<auth::Policy>,
    pub frontend: Option<frontend::Frontend>,
    pub legacy_ui_url: Option<String>,
    pub local_model: Option<local_model::Bridge>,
    pub intake: Option<intake::Service>,
    pub reader: Option<dataset_reader::Service>,
    pub index_store: Option<index_store::Store>,
    pub chat: tokio::sync::OnceCell<chat::Service>,
    pub public_chat_rust: bool,
}
impl Context {
    pub fn upstream_port(&self) -> Option<u16> {
        self.upstream.as_ref().and_then(reqwest::Url::port)
    }
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
    if context.upstream.is_none()
        && request.uri().path() == "/api/light/instance"
        && request.method() == axum::http::Method::GET
    {
        let instance_id = std::env::var("LES_LIGHT_INSTANCE_ID").unwrap_or_default();
        return json_response(
            StatusCode::OK,
            json!({"instance_id":instance_id}),
            "rust.launcher",
        );
    }
    if context.upstream.is_none() && request.method() == axum::http::Method::GET {
        match request.uri().path() {
            "/api/status" => return status::runtime(context, request.headers()).await,
            "/api/health" => return status::health(&context).await,
            "/api/jobs/summary" => return status::jobs(context, request).await,
            _ => {}
        }
    }
    #[cfg(feature = "edge")]
    if edge::handles(request.uri().path())
        && context
            .index_store
            .as_ref()
            .is_some_and(|s| s.edge_root.is_some())
    {
        return edge::handle(context, request).await;
    }
    if matches!(
        request.uri().path(),
        auth::POLICY_PATH
            | "/api/light/native-authorization"
            | "/api/light/native-admin-authorization"
    ) || intake::internal(request.uri().path())
    {
        return error(StatusCode::NOT_FOUND, "Not Found");
    }
    if request.uri().path() == local_model::PATH {
        return local_model::handle(&context, request).await;
    }
    if request.uri().path() == inference::PATH {
        return inference::handle(context, request).await;
    }
    if chat::http::handles(request.uri().path()) {
        return chat::http::handle(context, request).await;
    }
    if attachments::handles(request.uri().path()) {
        return attachments::handle(context, request).await;
    }
    if request.uri().path() == "/api/commands" {
        return chat::commands::handle(context, request).await;
    }
    if conversation::handles(request.uri().path()) {
        return conversation::handle(context, request).await;
    }
    if request.uri().path() == embedding_inference::PATH {
        return embedding_inference::handle(context, request).await;
    }
    if request.uri().path() == web::PATH {
        return web::handle(context, request).await;
    }
    if request.uri().path() == index_batch::PATH {
        return index_batch::handle(context, request).await;
    }
    if matches!(
        request.uri().path(),
        index_bootstrap::PATH | index_bootstrap::PRIVATE
    ) {
        return index_bootstrap::handle(context, request).await;
    }
    if request.uri().path() == dataset_reader::PRIVATE
        || dataset_reader::route(request.uri().path()).is_some()
    {
        return dataset_reader::handle(context, request).await;
    }
    if request.uri().path() == index_store::PATH {
        return index_store::handle(context, request).await;
    }
    if request.method() == "OPTIONS"
        && request.headers().contains_key("origin")
        && request
            .headers()
            .contains_key("access-control-request-method")
    {
        return auth::preflight(&context, request.headers()).await;
    }
    if matches!(
        request.uri().path(),
        chat::sources_api::PATH | chat::sources_api::TOOLS_PATH
    ) {
        return chat::sources_api::handle(context, request).await;
    }
    if context.public_chat_rust && chat::http::public_path(request.uri().path()) {
        return chat::http::handle_public(context, request).await;
    }
    if context.public_chat_rust
        && context.upstream.is_none()
        && mail::handles(request.method().as_str(), request.uri().path())
    {
        return mail::handle(context, request).await;
    }
    if context.public_chat_rust && attachments::public_path(request.uri().path()) {
        return attachments::handle_public(context, request).await;
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
        return frontend::configuration(
            &context,
            request.headers(),
            context.legacy_ui_url.as_deref(),
        )
        .await;
    }
    if let Some(route) = auth_keys::route(request.method(), request.uri().path()) {
        return auth_keys::handle(context, request, route).await;
    }
    if context.public_chat_rust
        && model_registry::settings::handles(request.method().as_str(), request.uri().path())
    {
        return model_registry::settings::handle(context, request).await;
    }
    if model_registry::handles(request.method().as_str(), request.uri().path()) {
        return model_registry::handle(context, request).await;
    }
    if let Some(route) = profiles::route(request.method().as_str(), request.uri().path()) {
        return profiles::handle(context, request, route).await;
    }
    if let Some(route) = mcp::route(request.method().as_str(), request.uri().path()) {
        return mcp::handle(context, request, route).await;
    }
    if history::handles(request.method().as_str(), request.uri().path()) {
        return history::handle(context, request).await;
    }
    if context.public_chat_rust && documents::browse::handles(&request) {
        return documents::browse::handle(context, request).await;
    }
    if matches!(request.method().as_str(), "GET" | "HEAD")
        && documents::parts::route(request.uri().path()).is_some()
    {
        return documents::parts::handle(context, request).await;
    }
    if matches!(request.method().as_str(), "GET" | "HEAD")
        && documents::route(request.uri().path()).is_some()
    {
        return documents::handle(context, request).await;
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
                | "--auth-policy-file"
                | "--edge-storage"
                | "--chat-owner"
        ) {
            return Err("Invalid native API argument");
        }
        let value = args.next().ok_or("Missing native API argument")?;
        if values.insert(key, value).is_some() {
            return Err("Duplicate native API argument");
        }
    }
    let public_chat_rust = match values.get("--chat-owner").map(String::as_str) {
        None | Some("legacy") => false,
        Some("rust") => true,
        _ => return Err("Invalid public chat owner"),
    };
    let listen: std::net::SocketAddr = values
        .get("--listen")
        .ok_or("Native API address missing")?
        .parse()
        .map_err(|_| "Invalid native API address")?;
    if listen.ip() != std::net::Ipv4Addr::LOCALHOST || listen.port() == 0 {
        return Err("Native API requires explicit IPv4 loopback port");
    }
    let upstream = values
        .get("--upstream")
        .map(|value| reqwest::Url::parse(value))
        .transpose()
        .map_err(|_| "Invalid legacy API address")?;
    if upstream.is_none() && !public_chat_rust {
        return Err("Legacy API address missing");
    }
    if upstream.as_ref().is_some_and(|upstream| {
        upstream.scheme() != "http"
            || upstream.host_str() != Some("127.0.0.1")
            || upstream.port().is_none()
            || upstream.path() != "/"
            || !upstream.username().is_empty()
            || upstream.password().is_some()
            || upstream.query().is_some()
            || upstream.fragment().is_some()
            || upstream.port() == Some(listen.port())
    }) {
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
        .map(|token| {
            local_model::Bridge::new(
                token.clone(),
                listen.port(),
                upstream.as_ref().and_then(reqwest::Url::port).unwrap_or(0),
            )
        })
        .transpose()?;
    let ingest_token = std::env::var("LES_INTERNAL_INGEST_TOKEN").ok();
    if ingest_token.as_ref().is_some_and(|token| {
        token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err("Invalid internal ingestion token");
    }
    let index_store = ingest_token
        .clone()
        .map(index_store::Store::from_env)
        .transpose()?
        .flatten();
    let index_store = if let Some(path) = values.get("--edge-storage") {
        #[cfg(not(feature = "edge"))]
        {
            let _ = path;
            return Err("This native API was built without Qdrant Edge");
        }
        #[cfg(feature = "edge")]
        {
            let token = ingest_token
                .clone()
                .ok_or("Edge requires an owned ingestion token")?;
            let key = std::env::var("LES_LIGHT_QDRANT_API_KEY")
                .map_err(|_| "Edge requires an owned storage key")?;
            Some(index_store::Store::embedded(
                std::path::Path::new(path),
                listen.port(),
                key,
                token,
            )?)
        }
    } else {
        index_store
    };
    let intake = ingest_token
        .filter(|_| std::env::var("LES_STARTUP_BACKGROUND_MUTATIONS").as_deref() == Ok("true"))
        .map(|_| intake::Service::new());
    let reader = intake.as_ref().map(|_| dataset_reader::Service::new());
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| "Native API client unavailable")?;
    let auth_policy = tokio::sync::OnceCell::new();
    if let Some(path) = values.get("--auth-policy-file") {
        auth_policy
            .set(auth::from_file(std::path::Path::new(path))?)
            .map_err(|_| "Native authorization policy already initialized")?;
    }
    Ok((
        listen,
        Context {
            listen_port: listen.port(),
            database,
            state_root,
            upstream,
            client,
            workspace_slots: Arc::new(tokio::sync::Semaphore::new(16)),
            attachment_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            upload_owner: tokio::sync::OnceCell::new(),
            auth_policy,
            frontend,
            legacy_ui_url,
            local_model,
            intake,
            reader,
            index_store,
            chat: tokio::sync::OnceCell::new(),
            public_chat_rust,
        },
    ))
}

fn main() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args == [std::ffi::OsString::from("--serve-chat-attachments")] {
        if let Err(code) = les_core::chat_attachments::serve_with(&document_dispatch::external) {
            eprintln!("{code}");
            std::process::exit(1);
        }
        return;
    }
    if args == [std::ffi::OsString::from("--serve-tables")] {
        if let Err(code) = les_core::tables::serve() {
            eprintln!("{code}");
            std::process::exit(1);
        }
        return;
    }
    if args == [std::ffi::OsString::from("--serve-document-formats")] {
        if let Err(code) = document_dispatch::serve() {
            eprintln!("{code}");
            std::process::exit(1);
        }
        return;
    }
    if args == [std::ffi::OsString::from("--serve-mcp")] {
        if let Err(code) = mcp::serve() {
            eprintln!("{code}");
            std::process::exit(1);
        }
        return;
    }
    if args == [std::ffi::OsString::from("--skill-host")] {
        if let Err(code) = les_core::skills::serve_once() {
            eprintln!("{code}");
            std::process::exit(1);
        }
        return;
    }
    if args == [std::ffi::OsString::from("--builtin-workbook")] {
        if let Err(code) = les_core::workbook_calculation::serve_once() {
            eprintln!("{code}");
            std::process::exit(1);
        }
        return;
    }
    if let [action, library] = args.as_slice() {
        if action == "--serve-embeddings" {
            if let Err(code) = embedding_worker::serve(std::path::Path::new(library)) {
                eprintln!(
                    "{}",
                    json!({"event":"embedding_worker_stopped","code":code})
                );
                std::process::exit(1);
            }
            return;
        }
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
            let result = pdf_text::extract(
                std::path::Path::new(library),
                std::path::Path::new(document),
            );
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
            "local_model_stream":"ollama-chat.v1","local_model_embeddings":"ollama-embed.v1",
            "canonical_embeddings":"rust.revision-pinned","local_onnx_embeddings":true,
            "embedded_edge":cfg!(feature="edge"),"skill_host":"rust.cpython-isolated","skill_runtime":"rust.verified-package","builtin_workbook":"rust.exact-decimal",
            "mcp_client":"rust.http-stdio","tool_execution_policy":"rust",
            "document_conversion":"rust.supervised","document_conversion_formats":["xlsx","xlsm","xls","csv","pptx","docx","doc","txt","md","json","jsonl","pdf"],
            "dataset_reader":"rust.durable-revision-pinned","dataset_reader_python_callbacks":false,
            "index_setup":"rust.durable-probe-plan","index_setup_python_callbacks":false,"lexical_schema":"rust",
            "index_batch_api":"rust.complete-or-refuse","index_batch_python_callbacks":false,
            "native_format_dispatch":true,"parquet":"rust.snappy","table_prepare":"rust"})
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
            if context.public_chat_rust && context.upstream.is_none() {
                mail::recover(&context.state_root);
            }
            let context = Arc::new(context);
            pdf_ocr::cleanup(&context.state_root);
            if context.public_chat_rust && context.upstream.is_none() {
                tokio::spawn(mail::schedule(context.clone()));
            }
            tokio::spawn(conversation::resume(context.clone()));
            let background = context.clone();
            if context.reader.is_some() {
                let reader = context.clone();
                tokio::spawn(async move {
                    dataset_reader::run(reader).await;
                });
            }
            tokio::spawn(async move {
                if index_store::start_recovery(background.clone())
                    .await
                    .is_ok()
                    && background.intake.is_some()
                {
                    intake::run(background).await;
                }
            });
            axum::serve(
                listener,
                Router::new().fallback(dispatch).with_state(context),
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
