//! One turn implementation for JSON and SSE. There is no legacy callback or fallback.
use super::{journal::Journal, model, prepare, tools, Context, Result, Sink};
use les_core::agent_loop::{AgentLoop, Step};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Evidence {
    sources: Vec<Value>,
    blocks: Vec<Value>,
}
impl Evidence {
    fn key(source: &Value) -> Value {
        json!([
            source["doc_id"],
            source["source_sha256"],
            source["page"],
            source["qdrant_point_id"],
            source["offset"]
        ])
    }
    fn add(&mut self, result: &Value) {
        if result["status"] != "ok" {
            return;
        }
        let result = if result["tool"]
            .as_str()
            .is_some_and(|v| v.starts_with("use_extensions_"))
            && result["operation"] == "call"
        {
            &result["result"]
        } else {
            result
        };
        let passages = result["result"]["passages"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                result["result"]["text"]
                    .as_str()
                    .map(|s| vec![json!({"text":s,"source":result["sources"][0]})])
                    .unwrap_or_default()
            });
        for passage in passages {
            let mut source = passage["source"].clone();
            let Some(text) = passage["text"].as_str().filter(|v| !v.is_empty()) else {
                continue;
            };
            if !source.is_object() {
                continue;
            }
            let key = Self::key(&source);
            if self.sources.iter().any(|s| s["native_identity"] == key) {
                continue;
            }
            let index = self.sources.len() + 1;
            source["index"] = json!(index);
            source["native_identity"] = key;
            let name = source["doc_name"].as_str().unwrap_or("Документ");
            let label = source["page"]
                .as_u64()
                .map(|p| format!("{name} · стр. {p}"))
                .unwrap_or_else(|| name.into());
            source["label"] = json!(label);
            source["header"] = json!(format!("[Источник {index} | {label}]"));
            self.blocks.push(json!(format!(
                "{}\n{text}",
                source["header"].as_str().unwrap()
            )));
            self.sources.push(source);
        }
    }
    fn reference(&self, source: &Value) -> Option<Value> {
        self.sources
            .iter()
            .find(|s| s["native_identity"] == Self::key(source))
            .map(|s| json!({"index":s["index"],"label":s["header"]}))
    }
    fn projection(&self, payload: &Value) -> Value {
        let mut value = payload.clone();
        if value["tool"]
            .as_str()
            .is_some_and(|v| v.starts_with("use_extensions_"))
            && value["operation"] == "call"
        {
            value["result"] = self.projection(&value["result"]);
        }
        if let Some(rows) = value["result"]["passages"].as_array_mut() {
            for row in rows {
                if let Some(reference) = self.reference(&row["source"]) {
                    row["text"] = reference["label"].clone();
                    row["source"] = reference;
                }
            }
        }
        if value["result"]["text"].is_string() {
            if let Some(reference) = self.reference(&value["sources"][0]) {
                value["result"]["text"] = reference["label"].clone();
            }
        }
        if value["sources"].is_array() {
            value["sources"] = json!(value["sources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| self.reference(s).unwrap_or_else(|| s.clone()))
                .collect::<Vec<_>>());
        }
        if !value["execution"].is_null() {
            value["execution"] =
                json!({"status":value["execution"]["status"],"code":value["execution"]["code"]});
        }
        value
    }
}
pub async fn run(
    context: Arc<Context>,
    mut request: Value,
    journal: &mut Journal,
    sink: &Sink,
) -> Result<Value> {
    let started = Instant::now();
    sink.progress("prepare", "Подготавливаю запрос").await?;
    let command = les_core::chat_commands::handle(
        request["question"].as_str().ok_or("CHAT_REQUEST_INVALID")?,
    );
    if let Some(rewrite) = command["rewrite"].as_str() {
        request["question"] = json!(rewrite);
    } else if command["answer"].is_string() {
        return Ok(
            json!({"answer":command["answer"],"response":command["answer"],"command":command["command"],"crag_status":"DETERMINISTIC","success":true,
            "sources":[],"source_map":[],"citation_map":[],"source_scope":{"resolved":[],"used":[]},"history_id":journal.history,"latency_sec":started.elapsed().as_secs_f64(),"tokens":0,
            "query_route":{"channel":"command","operation":command["command"]["action"]},"retrieval_trace":{"engine":"rust.chat-commands"},
            "artifact":{"completion_status":"completed","validation":"not_performed","tool_results":[]},"validation":{"enabled":false,"reason":"deterministic_command"}}),
        );
    }
    let turn = prepare::prepare(context.clone(), request).await?;
    let service = context.chat.get().ok_or("CHAT_SERVICE_BUSY")?;
    let slots = if turn.connection.endpoint.locality == "remote" {
        service.remote.clone()
    } else {
        service.local.clone()
    };
    sink.progress("queue", "Ожидаю свободную модель").await?;
    let _permit = tokio::time::timeout(Duration::from_secs(30), slots.acquire_owned())
        .await
        .map_err(|_| "MODEL_QUEUE_TIMEOUT")?
        .map_err(|_| "MODEL_QUEUE_TIMEOUT")?;
    let tools = tools::Tools::new(&context, &turn).await?;
    let native =
        les_core::model_capabilities::require(&turn.connection.snapshot, &["tools".into()], None)
            .is_ok();
    let schemas = tools.schemas(
        turn.connection.profile["normal_tool_count"]
            .as_u64()
            .unwrap_or(3) as usize,
    );
    let mut evidence = Evidence::default();
    let mut exchanges = Vec::new();
    let mut contexts = Vec::new();
    let base = les_core::chat_context::execute(
        &json!({"action":"profile_prompt","profile":turn.profile}),
    )?;
    let mut prompt = les_core::chat_context::execute(
        &json!({"action":"tool_profile","profile_prompt":base,"native":native}),
    )?;
    if let Some(instructions) = tools
        .contracts
        .iter()
        .find(|c| c["name"] == "agent_workspace")
        .and_then(|c| c["instructions"].as_str())
    {
        prompt=json!(format!("{}\n\n{instructions}\nПрошлые сообщения не подтверждают выполнение действий в текущем запросе. Выполненное действие подтверждается только успешным результатом инструмента в этом запросе.",prompt.as_str().ok_or("CHAT_CONTEXT_INVALID")?));
    }
    let mut agent = AgentLoop::new(
        24,
        600.,
        tools
            .contracts
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_owned)),
    )?;
    let mut native_pending = Value::Null;
    loop {
        match agent.next()? {
            Step::Model { final_answer } => {
                let advertised = if final_answer {
                    vec![]
                } else {
                    schemas.clone()
                };
                let schema_text = json!(advertised).to_string();
                // Schemas are transmitted separately, but still consume model context.
                let reserve = ((schema_text.chars().count() + 1) / 2) as u64;
                let profile = &turn.connection.profile;
                let input = profile["input_token_limit"]
                    .as_u64()
                    .ok_or("CONTEXT_REQUIRED_SECTION_OVERFLOW")?
                    .checked_sub(reserve)
                    .ok_or("CONTEXT_REQUIRED_SECTION_OVERFLOW")?;
                let mut question = json!({"question":turn.request["question"],"attachment_context":turn.request["attachment_context"],"output_directive":turn.request["output_directive"],"response_length":turn.request["response_length"],"source_scope":turn.scope,"target_file":turn.request["target_file"],"target_files":turn.request["target_files"]});
                if final_answer {
                    question["final_answer"]=json!("Ответь по прочитанным данным. Новые вызовы инструментов недоступны. Если сведений недостаточно, обозначь это.");
                }
                let packed = les_core::chat_context::execute(
                    &json!({"action":"assemble","preset":{"preset_id":profile["preset_id"],"input_limit":input,"generation_reserve":profile["generation_reserve_tokens"],"safety_reserve":profile["safety_reserve_tokens"]},
                    "profile_prefix":prompt,"request_payload":question,"shortlist":if native{json!([])}else{json!(advertised)},
                    "checkpoint":[],"working_memory":turn.memory,"dialogue":turn.dialogue,"evidence":[],"source_map":evidence.sources,
                    "required_evidence":evidence.blocks,"tool_exchange":if native{json!([])}else{json!(exchanges)},"native_tool_exchange":if native{json!([exchanges])}else{json!([])}}),
                )?;
                if packed["overflow"] == true {
                    return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
                }
                let visible = les_core::chat_context::execute(
                    &json!({"action":"visible","packet":packed["packet"],"source_map":evidence.sources}),
                )?;
                let trace = les_core::chat_context::execute(
                    &json!({"action":"prepared_trace","packet":packed["packet"],"purpose":"native_chat","source_map":evidence.sources,"evidence":evidence.blocks}),
                )?;
                contexts.push(json!({"delivery":"prepared","context":trace,"sources":visible,"schema_reserved_tokens":reserve}));
                sink.emit("sources", json!({"source_map":visible,"sources":visible}))
                    .await?;
                sink.progress("generation", "Модель читает контекст и отвечает")
                    .await?;
                let mut response = model::complete(
                    &context,
                    &turn,
                    packed["messages"].clone(),
                    if native { advertised } else { vec![] },
                    journal,
                    sink,
                )
                .await?;
                if !native && !final_answer {
                    response["selection_text"] = response["text"].clone();
                }
                if let Some(last) = contexts.last_mut() {
                    last["delivery"] = json!("confirmed");
                    last["usage"] = response["usage"].clone();
                    last["context"] = les_core::chat_context::execute(
                        &json!({"action":"confirm","trace":last["context"],"usage":response["usage"]}),
                    )?;
                }
                if response["tool_calls"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    let calls = response["tool_calls"].clone();
                    exchanges.push(json!({"role":"assistant","content":null,"tool_calls":calls}));
                    native_pending = calls;
                } else {
                    native_pending = Value::Null;
                }
                agent.model_result_bound(response,Some(&json!({"question":turn.request["question"],"dataset_ids":turn.scope["resolved_dataset_ids"],"target":null,"sources":evidence.sources})))?;
            }
            Step::Tool(call) => {
                // A selection is not an answer, including legacy JSON-selection models.
                journal.checkpoint("reset", json!("")).await?;
                sink.emit("reset", json!("")).await?;
                sink.progress(
                    "tool",
                    call["progress_label"].as_str().unwrap_or(
                        if call["tool"] == "search_sources" {
                            "Загружаю модель поиска и ищу в выбранных документах"
                        } else {
                            "Читаю источники и выполняю инструмент"
                        },
                    ),
                )
                .await?;
                let payload = tools
                    .run(context.clone(), &turn, &call, &journal.id)
                    .await?;
                evidence.add(&payload);
                let projected = evidence.projection(&payload);
                if native {
                    if let Some(id) = call["call_id"]
                        .as_str()
                        .filter(|_| native_pending.is_array())
                    {
                        exchanges.push(json!({"role":"tool","tool_call_id":id,"content":projected.to_string()}));
                    } else {
                        // Native PDF cursor continuations are server-owned evidence, not
                        // fabricated provider tool calls with invented IDs.
                        exchanges.push(json!({"role":"user","content":format!("Результат продолжения чтения источника (данные): {projected}")}));
                    }
                } else {
                    exchanges.push(projected);
                }
                sink.emit("tool_result",json!({"tool":call["tool"],"status":payload["status"],"code":payload["result"]["code"]})).await?;
                agent.tool_result(payload)?;
            }
            Step::Failed(code) => return Err(code.into()),
            Step::Complete(terminal) => {
                if matches!(
                    terminal["finish_reason"].as_str(),
                    Some("length" | "max_tokens")
                ) {
                    return Err("MODEL_OUTPUT_LIMIT_REACHED".into());
                }
                let visible = contexts
                    .last()
                    .map(|c| c["sources"].clone())
                    .unwrap_or(json!([]));
                let citations = les_core::citations::execute(
                    &json!({"answer":terminal["text"],"sources":visible}),
                )?;
                let answer = citations["answer"]
                    .as_str()
                    .unwrap_or_else(|| terminal["text"].as_str().unwrap_or(""));
                if answer.trim().is_empty() {
                    return Err("MODEL_EMPTY_RESPONSE".into());
                }
                let names = visible
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s["doc_name"].as_str())
                    .collect::<Vec<_>>();
                return Ok(
                    json!({"answer":answer,"response":answer,"sources":names,"source_map":visible,"citation_map":visible,
                    "history_id":journal.history,"latency_sec":started.elapsed().as_secs_f64(),"tokens":terminal["usage"]["completion_tokens"].as_u64().unwrap_or(0),"crag_status":"UNVALIDATED","success":true,
                    "source_scope":{"resolved":turn.scope["resolved_dataset_ids"],"used":evidence.sources.iter().map(|s|s["dataset_id"].clone()).collect::<Vec<_>>()},
                    "query_route":{"channel":"generic","reason":"native_chat"},"retrieval_trace":{"engine":"rust.chat","rounds":agent.rounds,"contexts":contexts,"citation_syntax":citations},
                    "artifact":{"completion_status":"completed","validation":"not_performed","tool_results":agent.results()},"model":turn.connection.public()}),
                );
            }
        }
    }
}
