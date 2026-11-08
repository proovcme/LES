//! The complete first-index transaction: exact role, probe, layout, durable plan, contract.
use crate::index_batch::configuration::{self, Settings};
use crate::{auth, error, json_response, library::catalog, model_runtime, Context};
use axum::{body::to_bytes, extract::Request, http::StatusCode, response::Response};
use les_core::index_bootstrap::{self as core, Session};
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Arc;
pub const PATH: &str = "/api/rag/index/setup";
pub const PRIVATE: &str = "/api/light/internal-index-setup";

pub(crate) fn message(key: &str) -> &'static str {
    match key {
        "EMBEDDING_MODEL_NOT_ASSIGNED"=>"Назначьте модель поиска в настройках Леса.",
        "INDEX_ADOPTION_REQUIRED"=>"Найден существующий индекс без контракта. Подтвердите проверку и принятие индекса.",
        "INDEX_CATALOG_MISMATCH"=>"Каталог содержит проиндексированные документы, но хранилище неполное. Требуется восстановление индекса.",
        "EMBEDDING_INDEX_DIMENSION_MISMATCH"=>"Размерность модели поиска отличается от настройки или существующего индекса. Требуется явная миграция.",
        "EMBEDDING_INDEX_MODEL_MISMATCH"|"INDEX_CONTRACT_MISMATCH"=>"Назначенная модель или параметры отличаются от индекса. Проверьте подключение; существующие данные сохранены.",
        "INDEX_UPDATE_BUSY"=>"Индекс сейчас обновляется. Повторите после завершения обработки.",
        "DOCUMENT_TOKENIZER_UNAVAILABLE"=>"Не найден локальный токенизатор модели поиска. Укажите его файл или явно выберите нарезку по символам.",
        "INDEX_SCHEMA_MIGRATION_REQUIRED"=>"Структура индекса несовместима с настройкой. Требуется явная миграция.",
        "INDEX_MEMORY_GUARD"=>"Для проверки модели поиска освободите оперативную память: требуется не менее 4 ГБ.",
        "INDEX_CONTRACT_INVALID"=>"Файл описания индекса повреждён. Восстановите его из резервной копии; Лес не перезапишет его автоматически.",
        "EDGE_STORAGE_NOT_EMPTY"=>"В папке индекса есть посторонние файлы. Выберите отдельную пустую папку для хранилища Леса.",
        "INDEX_SETUP_MODEL_CHANGED"|"INDEX_SETUP_PLAN_CHANGED"=>"Модель или настройки изменились во время создания индекса. Верните прежнее назначение и повторите восстановление.",
        value if ["MODEL_","CONNECTION_","UPSTREAM_","INFERENCE_","EMBEDDING_","CAPABILITY_"].iter().any(|prefix|value.starts_with(prefix))=>"Модель поиска не готова или не вернула корректные векторы. Проверьте подключение модели и повторите действие.",
        _=>"Не удалось подготовить индекс. Проверьте модель поиска и журнал обработки, затем повторите действие.",
    }
}
fn stop(code: &str) -> Response {
    let key = code.split(':').next().unwrap_or(code);
    let detail = message(key);
    let upstream_status = if key == "UPSTREAM_HTTP_ERROR" {
        code.split_once(':')
            .and_then(|(_, v)| v.trim().parse::<u16>().ok())
            .filter(|n| (100..=599).contains(n))
    } else {
        None
    };
    eprintln!(
        "{}",
        json!({"event":"index_setup_stopped","code":key,"upstream_status":upstream_status})
    );
    json_response(
        if key == "INDEX_UPDATE_BUSY" {
            StatusCode::CONFLICT
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        json!({"detail":detail,"code":key,"upstream_status":upstream_status}),
        "rust.index-setup",
    )
}
fn selected(context: &Context) -> Result<Value, &'static str> {
    let result = catalog(
        &context.database,
        "model_execution_snapshot",
        json!({"role":"embeddings"}),
    );
    if result["status"] != "completed" {
        return Err("MODEL_REGISTRY_UNAVAILABLE");
    }
    if result["value"].is_null() {
        return Err("EMBEDDING_MODEL_NOT_ASSIGNED");
    }
    Ok(result["value"].clone())
}
fn dimension(bytes: &[u8], model: &str) -> Result<u64, &'static str> {
    let split = bytes
        .iter()
        .position(|b| *b == b'\n')
        .ok_or("EMBEDDING_RESPONSE_INVALID")?;
    let header: Value =
        serde_json::from_slice(&bytes[..split]).map_err(|_| "EMBEDDING_RESPONSE_INVALID")?;
    let dimension = header["dimension"]
        .as_u64()
        .filter(|n| (1..=65536).contains(n))
        .ok_or("EMBEDDING_RESPONSE_INVALID")?;
    if header["model_id"]
        .as_str()
        .is_some_and(|v| core::model_key(v) != core::model_key(model))
    {
        return Err("EMBEDDING_INDEX_MODEL_MISMATCH");
    }
    if header["count"] != 1 || bytes.len() - split - 1 != dimension as usize * 8 {
        return Err("EMBEDDING_RESPONSE_INVALID");
    }
    let mut norm = 0.;
    for word in bytes[split + 1..].chunks_exact(8) {
        let number = f64::from_le_bytes(word.try_into().map_err(|_| "EMBEDDING_RESPONSE_INVALID")?);
        if !number.is_finite() {
            return Err("EMBEDDING_RESPONSE_INVALID");
        }
        norm += number * number;
    }
    if !norm.is_finite() || norm < 1e-24 {
        return Err("EMBEDDING_RESPONSE_INVALID");
    }
    Ok(dimension)
}
fn compatible(info: &Value, contract: &Value) -> bool {
    let params = &info["config"]["params"];
    let dense = contract["dense_vector_name"].as_str().unwrap_or("");
    let sparse = contract["sparse_vector_name"].as_str().unwrap_or("");
    params["vectors"][dense]["size"] == contract["vector_size"]
        && params["vectors"][dense]["distance"]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case("cosine"))
        && params["sparse_vectors"].get(sparse).is_some()
}

pub(crate) async fn ensure(
    context: Arc<Context>,
    allow_remote: bool,
    adopt: bool,
    replace_empty: bool,
    expected: Option<String>,
    for_batch: bool,
) -> Result<Value, String> {
    let store = context
        .index_store
        .as_ref()
        .ok_or("INDEX_STORAGE_UNAVAILABLE")?;
    if let Some(code) = store.recovery_error() {
        if !matches!(code, "INDEX_NOT_CONFIGURED" | "INDEX_RECOVERING") {
            return Err(code.into());
        }
    }
    let permit = context
        .workspace_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| "INDEX_UPDATE_BUSY")?;
    let runtime = tokio::runtime::Handle::current();
    let worker = context.clone();
    let result=tokio::task::spawn_blocking(move||->Result<Value,String>{
        let _permit=permit;
        let context=worker;
        let store=context.index_store.as_ref().ok_or("INDEX_STORAGE_UNAVAILABLE")?;
        let settings=Settings::load(&context.state_root)?;
        let (collection,path)=configuration::identity(&context,&settings)?;
        if expected.as_ref().is_some_and(|v|v!=&collection){return Err("INDEX_COLLECTION_MISMATCH".into());}
        les_core::lexical_schema::initialize(&context.database)?;
        runtime.block_on(store.recover_setup(&context,&collection))?;
        let session=Session::open(&context.database,&store.content_root(&context),&collection,&path)?;
        let operation=(||->Result<Value,String>{
            let saved=core::read(&path)?;
            let pending=session.pending()?;
            let binding=selected(&context)?;
            let env=std::env::var_os("LES_ENV_PATH").map(std::path::PathBuf::from).unwrap_or_else(||context.state_root.join(".env"));
            let resolved=model_runtime::resolve(&binding,"embeddings",&env)?;
            let revision=resolved.revision["revision_id"].as_str().ok_or("MODEL_REGISTRY_UNAVAILABLE")?;
            let model=resolved.revision["model_id"].as_str().ok_or("MODEL_REGISTRY_UNAVAILABLE")?;
            let initial=runtime.block_on(store.setup_call(&context,&collection,Method::GET,"",Value::Null))?;
            let count=if initial.is_some(){runtime.block_on(store.setup_call(&context,&collection,Method::POST,"points/count",json!({"exact":true})))?.and_then(|v|v["count"].as_u64()).ok_or("INDEX_POINT_COUNT_UNAVAILABLE")?}else{0};
            if count<session.indexed_count()?{return Err("INDEX_CATALOG_MISMATCH".into());}
            if saved.is_none() && pending.is_none() && initial.is_some() && !adopt {return Err("INDEX_ADOPTION_REQUIRED".into());}
            let measured=if pending.is_none() && (saved.is_none() || initial.is_none()) {
                match les_core::system_memory::available(){Ok(n)if n>=4*1024*1024*1024=>(),Ok(_)=>return Err("INDEX_MEMORY_GUARD".into()),Err(code)=>return Err(code.into())}
                session.probe(revision)?;
                eprintln!("{}",json!({"event":"index_setup_probe_started","model_revision":revision}));
                let bytes=runtime.block_on(crate::embedding_inference::execute(context.clone(),&json!({"schema":"les.embedding-inference.v1","revision_id":revision,
                    "inputs":["LES embedding dimension verification"],"allow_remote":allow_remote,"timeout_seconds":90,"release_after":!for_batch})))?;
                Some(dimension(&bytes,model)?)
            }else{None};
            let contract=if let Some(contract)=saved.as_ref().or(pending.as_ref()) {
                if saved.is_none() && contract["native_embedding_revision"]!=revision {return Err("INDEX_SETUP_MODEL_CHANGED".into());}
                if core::model_key(contract["embedding_model"].as_str().unwrap_or(""))!=core::model_key(model){return Err("EMBEDDING_INDEX_MODEL_MISMATCH".into());}
                if measured.is_some_and(|n|contract["vector_size"].as_u64()!=Some(n)){return Err("EMBEDDING_INDEX_DIMENSION_MISMATCH".into());}
                les_core::index_configuration::execute(&json!({"contract":contract,"settings":settings.core_values(),"tokenizer":configuration::tokenizer(&settings,contract)?}))?;
                contract.clone()
            } else {
                let actual=measured.ok_or("EMBEDDING_RESPONSE_INVALID")?;
                let backend=if std::env::var_os("LES_NATIVE_EMBEDDING_SPEC").is_some(){"sentence_transformers"}else if resolved.snapshot["transport_options"]["embedding_protocol"]=="native_embedding_v1"{"ollama"}else{"openai"};
                let tokenizer=configuration::tokenizer(&settings,&json!({"chunk_unit":settings.get("RAG_CHUNK_UNIT","tokens"),"embedding_model":model}))?;
                core::contract(&settings.core_values(),model,revision,actual,backend,tokenizer)?
            };
            if let Some(info)=&initial {
                if !compatible(info,&contract) {
                    if count>0 || !replace_empty{return Err("INDEX_SCHEMA_MIGRATION_REQUIRED".into());}
                } else if saved.is_none() && count>0 {
                    let matched=runtime.block_on(store.setup_call(&context,&collection,Method::POST,"points/count",json!({"exact":true,"filter":{"must":[{"key":"embedding_fingerprint","match":{"value":contract["point_embedding_fingerprint"]}}]}})))?.and_then(|v|v["count"].as_u64()).ok_or("INDEX_POINT_COUNT_UNAVAILABLE")?;
                    if matched!=count{return Err("INDEX_ADOPTION_UNVERIFIED".into());}
                }
            }
            // Persist the exact plan before any creation. A lost response to PUT
            // can then be resumed without pretending an unrelated index is ours.
            if selected(&context)?!=binding || Settings::load(&context.state_root)?.core_values()!=settings.core_values(){return Err("INDEX_SETUP_MODEL_CHANGED".into());}
            session.plan(&contract,revision)?;
            let created=initial.as_ref().is_none_or(|info|!compatible(info,&contract));
            if created {
                if initial.is_some(){runtime.block_on(store.setup_call(&context,&collection,Method::DELETE,"",Value::Null))?;}
                eprintln!("{}",json!({"event":"index_setup_creating","model_revision":revision}));
                if !store.setup_edge(&contract)? {
                    let dense=contract["dense_vector_name"].as_str().ok_or("INDEX_CONTRACT_INVALID")?;
                    let sparse=contract["sparse_vector_name"].as_str().ok_or("INDEX_CONTRACT_INVALID")?;
                    let result=runtime.block_on(store.setup_call(&context,&collection,Method::PUT,"",json!({"vectors":{dense:{"size":contract["vector_size"],"distance":"Cosine"}},"sparse_vectors":{sparse:{"modifier":"idf"}}})))?;
                    if result!=Some(json!(true)){return Err("INDEX_STORAGE_RESPONSE_INVALID".into());}
                }
                let info=runtime.block_on(store.setup_call(&context,&collection,Method::GET,"",Value::Null))?.ok_or("INDEX_STORAGE_RESPONSE_INVALID")?;
                if !compatible(&info,&contract){return Err("INDEX_SCHEMA_MIGRATION_REQUIRED".into());}
            }
            let latest=selected(&context)?;
            if latest!=binding || Settings::load(&context.state_root)?.core_values()!=settings.core_values(){return Err("INDEX_SETUP_MODEL_CHANGED".into());}
            store.finish_setup_held(session.writer())?;
            if selected(&context)?!=binding || Settings::load(&context.state_root)?.core_values()!=settings.core_values(){return Err("INDEX_SETUP_MODEL_CHANGED".into());}
            session.finish(&contract)?;
            store.mark_setup_ready();
            Ok(json!({"status":"ready","collection":collection,"vector_size":contract["vector_size"],"contract_fingerprint":contract["fingerprint"],"created":created,"resumed":pending.is_some(),"model_revision":revision}))
        })();
        if let Err(code)=&operation {session.failed(code);}
        operation
    }).await.map_err(|_|"INDEX_SETUP_OWNER_FAILED")?;
    if result.is_err() && for_batch {
        let _ = tokio::task::spawn_blocking(les_core::release_batch_workers).await;
    }
    let result = result?;
    let auxiliary = context.clone();
    let collection = result["collection"]
        .as_str()
        .ok_or("INDEX_STORAGE_RESPONSE_INVALID")?
        .to_owned();
    tokio::task::spawn_blocking(move || {
        let Ok(settings) = Settings::load(&auxiliary.state_root) else {
            return;
        };
        let yes = |key| {
            ["1", "true", "yes", "on"].contains(&settings.get(key, "true").to_lowercase().as_str())
        };
        if !yes("LES_RAG_PAYLOAD_INDEX_ENSURE") || !yes("LES_STARTUP_BACKGROUND_MUTATIONS") {
            return;
        }
        let Some(store) = auxiliary.index_store.as_ref() else {
            return;
        };
        let runtime = tokio::runtime::Handle::current();
        let info = runtime.block_on(store.setup_call(
            &auxiliary,
            &collection,
            Method::GET,
            "",
            Value::Null,
        ));
        let Ok(Some(info)) = info else {
            return;
        };
        for field in [
            "dataset_id",
            "file_name",
            "embedding_fingerprint",
            "mail_account_id",
            "mail_thread_key",
            "mail_registry_message_id",
        ] {
            if info["payload_schema"].get(field).is_some() {
                continue;
            }
            if let Err(code) = runtime.block_on(store.setup_call(
                &auxiliary,
                &collection,
                Method::PUT,
                "index",
                json!({"field_name":field,"field_schema":"keyword"}),
            )) {
                eprintln!(
                    "{}",
                    json!({"event":"index_payload_index_deferred","field":field,"code":code})
                );
            }
        }
    });
    eprintln!(
        "{}",
        json!({"event":"index_setup_completed","created":result["created"],"resumed":result["resumed"]})
    );
    Ok(result)
}

pub async fn handle(context: Arc<Context>, request: Request) -> Response {
    let private = request.uri().path() == PRIVATE;
    let cors = if private {
        if context
            .index_store
            .as_ref()
            .is_none_or(|s| !s.authorized_internal(request.headers()))
        {
            return error(StatusCode::NOT_FOUND, "Not Found");
        }
        axum::http::HeaderMap::new()
    } else {
        match auth::authorize(&context, request.headers(), true).await {
            Ok(cors) => cors,
            Err(error) => return error,
        }
    };
    if !private && request.method() == "GET" {
        let owner = context.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, &'static str> {
            let settings = Settings::load(&owner.state_root)?;
            let (collection, path) = configuration::identity(&owner, &settings)?;
            let mut status = core::status(&owner.database, &collection)?;
            status["collection"] = json!(collection);
            status["contract_present"] = json!(core::read(&path)?.is_some());
            Ok(status)
        })
        .await;
        let mut response = match result {
            Ok(Ok(value)) => json_response(StatusCode::OK, value, "rust.index-setup"),
            Ok(Err(code)) => stop(code),
            Err(_) => stop("INDEX_SETUP_OWNER_FAILED"),
        };
        response.headers_mut().extend(cors);
        return response;
    }
    if request.method() != "POST" {
        return error(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed");
    }
    let bytes = match to_bytes(request.into_body(), 16384).await {
        Ok(b) => b,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "INDEX_SETUP_INVALID"),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(Value::Object(v)) => Value::Object(v),
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_SETUP_INVALID"),
    };
    if body
        .as_object()
        .unwrap()
        .iter()
        .any(|(key, value)| match key.as_str() {
            "allow_remote" | "adopt_existing" | "replace_empty" => !value.is_boolean(),
            "collection" => !value.is_string(),
            _ => true,
        })
    {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "INDEX_SETUP_INVALID");
    }
    let mut response = match ensure(
        context,
        body["allow_remote"] == true,
        body["adopt_existing"] == true,
        body["replace_empty"] == true,
        body["collection"].as_str().map(str::to_owned),
        false,
    )
    .await
    {
        Ok(value) => json_response(StatusCode::OK, value, "rust.index-setup"),
        Err(code) => stop(&code),
    };
    response.headers_mut().extend(cors);
    response
}
