//! Revision-safe model-selected memory. No Python model/scheduler callbacks.
mod http;
mod scheduler;
use crate::{model_runtime, Context};
pub use http::{handle, handles};
pub use scheduler::resume;
pub use scheduler::{enqueue, start, Foreground, Scheduler};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
type Result<T> = std::result::Result<T, String>;
async fn catalog(context: &Context, action: &'static str, args: Value) -> Result<Value> {
    let db = context.database.clone();
    tokio::task::spawn_blocking(move || {
        let result = crate::library::catalog(&db, action, args);
        if result["status"] != "completed" {
            return Err(result["code"]
                .as_str()
                .unwrap_or("SUMMARY_STORAGE_UNAVAILABLE")
                .into());
        }
        Ok(result["value"].clone())
    })
    .await
    .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE".to_owned())?
}
pub async fn job(context: &Context, action: &'static str, args: Value) -> Result<Value> {
    let db = context.database.clone();
    tokio::task::spawn_blocking(move || {
        les_core::conversation_summary::queue::execute(&db, action, &args).map_err(str::to_owned)
    })
    .await
    .map_err(|_| "SUMMARY_STORAGE_UNAVAILABLE".to_owned())?
}
async fn binding(context: Arc<Context>) -> Result<model_runtime::Resolved> {
    tokio::task::spawn_blocking(move || crate::chat::prepare::answer(&context).map_err(|e| e.code))
        .await
        .map_err(|_| "SUMMARY_MODEL_UNAVAILABLE".to_owned())?
}
async fn update(
    context: &Context,
    sid: &str,
    state: &Value,
    changes: Value,
    claim: Option<&Value>,
    retry: bool,
    code: &str,
) -> Result<Value> {
    if let Some(claim) = claim {
        job(context,"commit",json!({"session_id":sid,"claim_token":claim["claim_token"],"expected_revision":state["revision"],"changes":changes,"retry":retry,"code":code})).await
    } else {
        catalog(
            context,
            "context_update",
            json!({"session_id":sid,"expected_revision":state["revision"],"changes":changes}),
        )
        .await
    }
}
pub async fn execute(
    context: Arc<Context>,
    sid: &str,
    force: bool,
    expected: Option<i64>,
    claim: Option<&Value>,
) -> Result<Value> {
    let db = context.database.clone();
    let session = sid.to_owned();
    let registered=tokio::task::spawn_blocking(move||crate::chat::unwrap(les_core::workspace::execute(&json!({"schema":"les.workspace.v1","database":db,"action":"get","arguments":{"session_id":session}}))).map_err(|e|e.code)).await.map_err(|_|"SUMMARY_STORAGE_UNAVAILABLE".to_owned())??;
    let state = catalog(&context, "context_get", json!({"session_id":sid})).await?;
    if expected.is_some_and(|r| state["revision"].as_i64() != Some(r)) {
        return Err("SUMMARY_CONTEXT_CHANGED".into());
    }
    if registered["registered"] != true {
        if expected.is_some() {
            return Err("SUMMARY_SESSION_NOT_FOUND".into());
        }
        return Ok(state);
    }
    let turns=catalog(&context,"context_summary_history",json!({"session_id":sid,"after_id":state["through_id"].as_i64().unwrap_or(0).max(state["cutoff_id"].as_i64().unwrap_or(0))})).await?;
    let rough = les_core::conversation_summary::execute(
        &json!({"action":"needed","state":state,"turns":turns,"force":force}),
    )
    .map_err(str::to_owned)?;
    if rough["needed"] != true {
        return Ok(state);
    }
    let result=async{
        let connection=binding(context.clone()).await?;
        let service=context.chat.get().ok_or("SUMMARY_SERVICE_UNAVAILABLE")?;
        let slots=if connection.endpoint.locality=="remote"{service.remote.clone()}else{service.local.clone()};
        let _permit=tokio::time::timeout(Duration::from_secs(30),slots.acquire_owned()).await.map_err(|_|"MODEL_QUEUE_TIMEOUT")?.map_err(|_|"MODEL_QUEUE_TIMEOUT")?;
        let plan=les_core::conversation_summary::execute(&json!({"action":"plan","state":state,"turns":turns,"force":force,"input_token_limit":connection.profile["input_token_limit"]})).map_err(str::to_owned)?;
        let request=json!({"messages":plan["messages"],"max_output_tokens":650,"temperature":0,"tools":[],"reasoning_enabled":false,"response_format":null});
        let native=connection.snapshot["transport_options"]["chat_protocol"]=="native_chat_v1";
        let streaming=native||les_core::model_capabilities::require(&connection.snapshot,&["streaming".into()],None).is_ok();
        let upstream=context.upstream_port().unwrap_or(0);
        let body=connection.chat(&request,streaming,Duration::from_secs(120),[context.listen_port,upstream]).await?;
        let text=tokio::time::timeout(Duration::from_secs(180),crate::model_answer::collect(body,"SUMMARY")).await.map_err(|_|"SUMMARY_MODEL_TIMEOUT")??;
        let selected=les_core::conversation_summary::execute(&json!({"action":"select","response":text,"records":plan["records"],"finish_reason":"stop"})).map_err(str::to_owned)?;
        update(&context,sid,&state,json!({"summary":selected["summary"],"through_id":plan["through_id"],"model_revision":connection.revision["revision_id"],"last_error":""}),claim,plan["continue"]==true,"").await
    }.await;
    match result {
        Ok(value) => Ok(value),
        Err(code)
            if matches!(
                code.as_str(),
                "SUMMARY_CONTEXT_CHANGED" | "WORKSPACE_CONFLICT"
            ) =>
        {
            Err("SUMMARY_CONTEXT_CHANGED".into())
        }
        Err(code) => {
            let safe = code
                .split(':')
                .next()
                .filter(|s| {
                    s.len() <= 100
                        && s.bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                })
                .unwrap_or("SUMMARY_UPDATE_FAILED");
            eprintln!(
                "{}",
                json!({"event":"conversation_summary_failed","code":safe})
            );
            let message = if code == "SUMMARY_TURN_TOO_LARGE" {
                "Сообщение слишком длинное для выбранной модели. Сохраните важное отдельной записью памяти."
            } else if code == "SUMMARY_SELECTION_INVALID" || code == "SUMMARY_MODEL_TRUNCATED" {
                "Модель не завершила краткую сводку. История сохранена; попробуйте ещё раз."
            } else {
                "Сводку пока не удалось обновить. История сохранена; проверьте подключение модели."
            };
            update(
                &context,
                sid,
                &state,
                json!({"last_error":message}),
                claim,
                false,
                safe,
            )
            .await
        }
    }
}
