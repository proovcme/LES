//! One turn implementation for JSON and SSE. There is no legacy callback or fallback.
use super::{
    evidence::Evidence,
    journal::Journal,
    model, prepare,
    protocol::{bound_pair, complete_pending, effective, proposal, strip_bound_calls},
    tools, Context, Result, Sink,
};
use les_core::agent_loop::{AgentLoop, Step};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub async fn run(
    context: Arc<Context>,
    request: &mut Value,
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
    let mut turn = prepare::prepare(context.clone(), request.clone()).await?;
    request["attachment_context"] = turn.request["attachment_context"].clone();
    if !turn.attachment.is_null() {
        journal
            .checkpoint("attachment_context", request["attachment_context"].clone())
            .await?;
    }
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
    let mut tools = tools::Tools::new(&context, &turn).await?;
    tools.bind_table(super::table_document::TableTool::bind(&context, &turn).await?);
    let available = turn.connection.profile["normal_tool_count"]
        .as_u64()
        .unwrap_or(3)
        > 1 + u64::from(tools.table.is_some())
        && turn.profile["tools"].as_array().is_some_and(|v| {
            v.iter()
                .any(|n| n == "search_sources" || n == "read_source" || n == "document_review")
        });
    tools.bind_review(super::document_review::ReviewTool::bind(&context, &turn, available).await?);
    if let Some(table) = &tools.table {
        turn.request["attachment_context"] = json!(table.manifest().to_string());
        request["attachment_context"] = turn.request["attachment_context"].clone();
        journal
            .checkpoint("attachment_context", request["attachment_context"].clone())
            .await?;
    }
    let native =
        les_core::model_capabilities::require(&turn.connection.snapshot, &["tools".into()], None)
            .is_ok();
    let mut disclosed = Vec::new();
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
    let mut table_result = Value::Null;
    let mut review_result = Value::Null;
    let mut table_proposal = Value::Null;
    let mut review_proposal = Value::Null;
    loop {
        match agent.next()? {
            Step::Model { final_answer } => {
                if native {
                    complete_pending(&mut exchanges, agent.stop_reason());
                }
                let mut required = evidence.blocks.clone();
                let mut native_groups = Vec::new();
                if native && !exchanges.is_empty() {
                    native_groups.push(json!(exchanges));
                }
                if let Some(table) = &tools.table {
                    let state = table.state().await?;
                    agent
                        .update_epochs(json!({"table_document":state["table_task"]["revision"]}))?;
                    let state =
                        json!({"table_document_state":state,"last_table_result":table_result});
                    if native && table_proposal.is_object() {
                        native_groups.push(bound_pair(&table_proposal, state));
                    } else {
                        required.push(state);
                    }
                }
                if let Some(review) = &tools.review {
                    let view = review.context(context.clone(), &turn).await?;
                    agent.capability_revision("document_review", review.revision().await)?;
                    if native && review_proposal.is_object() {
                        required.push(json!(evidence.review_header(&view)));
                        native_groups.push(bound_pair(
                            &review_proposal,
                            json!({"document_state":view,"last_review_result":review_result}),
                        ));
                    } else {
                        required.push(evidence.review_context(&view)?);
                        required.push(json!({"last_review_result":review_result}));
                    }
                    review.assert_current(context.clone(), &turn).await?;
                }
                let mut advertised = if final_answer {
                    vec![]
                } else {
                    tools.disclosed_schemas(
                        turn.connection.profile["normal_tool_count"]
                            .as_u64()
                            .unwrap_or(3) as usize,
                        turn.scope["document_grounding_enabled"] == true,
                        &disclosed,
                    )
                };
                if let Some(review) = &tools.review {
                    for schema in &mut advertised {
                        if schema["function"]["name"] == "document_review" {
                            schema["function"]["parameters"] = review.schema().await;
                        }
                    }
                }
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
                    "required_evidence":required,"tool_exchange":if native{json!([])}else{json!(exchanges)},"native_tool_exchange":native_groups}),
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
                if let Some(review) = &tools.review {
                    review.acknowledge(packed["messages"].clone()).await?;
                    agent.capability_revision("document_review", review.revision().await)?;
                }
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
                journal.tool_result(payload.clone()).await?;
                if let Some(name) = tools.disclosed(&payload) {
                    disclosed.retain(|n| n != &name);
                    disclosed.push(name);
                    if disclosed.len() > 128 {
                        disclosed.remove(0);
                    }
                }
                evidence.add(&payload);
                let projected = evidence.projection(&payload);
                let effect = effective(&payload);
                if effect["tool"] == "table_document" || effect["tool"] == "document_review" {
                    if effect["tool"] == "table_document" {
                        table_result = json!({"arguments":call["args"],"result":projected});
                        table_proposal = proposal(&native_pending, &call);
                    } else {
                        review_result = json!({"arguments":call["args"],"result":projected});
                        review_proposal = proposal(&native_pending, &call);
                    }
                    // Row packets live in the ledger. Keep no growing table transcript
                    // in the model context, including mixed provider tool-call groups.
                    if native {
                        strip_bound_calls(&mut exchanges, effect["tool"].as_str().unwrap());
                    }
                } else if native {
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
                if visible
                    .as_array()
                    .is_some_and(|v| v.iter().any(|s| s["evidence_kind"] == "physical_pdf_page"))
                {
                    sink.progress("verification", "Проверяю источники ответа")
                        .await?;
                    tools::sources::verify_physical(context.clone(), &visible).await?;
                }
                let citations = les_core::citations::execute(
                    &json!({"answer":terminal["text"],"sources":visible}),
                )?;
                let answer = citations["answer"]
                    .as_str()
                    .unwrap_or_else(|| terminal["text"].as_str().unwrap_or(""));
                if answer.trim().is_empty() {
                    return Err("MODEL_EMPTY_RESPONSE".into());
                }
                let receipt = if let Some(table) = &tools.table {
                    table.receipt().await?
                } else {
                    Value::Null
                };
                let review_receipt = if let Some(review) = &tools.review {
                    review.receipt(context.clone(), &turn).await?
                } else {
                    Value::Null
                };
                let mut receipts = vec![receipt.clone(), review_receipt]
                    .into_iter()
                    .filter(|r| !r.is_null())
                    .collect::<Vec<_>>();
                for result in agent.results().iter().map(effective).filter(|p| {
                    p["tool"] == "agent_workspace"
                        && ["save", "edit", "delete", "restore"]
                            .iter()
                            .any(|op| p["operation"] == *op)
                }) {
                    receipts.push(
                        les_core::execution_receipt(
                            &json!({"action":"local_file_receipt","payload":result}),
                        )?["receipt"]
                            .clone(),
                    );
                }
                let summary = les_core::execution_receipt(
                    &json!({"action":"receipt_summary","receipts":receipts}),
                )?["summary"]
                    .clone();
                let receipt = receipts.first().cloned().unwrap_or(Value::Null);
                let answer = if !summary.is_null() {
                    format!(
                        "{}\n\n{answer}",
                        summary["notice"].as_str().ok_or("REVIEW_TASK_INVALID")?
                    )
                } else {
                    answer.to_owned()
                };
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
                    "artifact":{"completion_status":if !summary.is_null()&&summary["status"]!="recorded"{"incomplete"}else{"completed"},"execution_receipt":receipt,"execution_summary":summary,"validation":"not_performed","tool_results":agent.results(),"attachment":{
                        "attachment_id":turn.attachment["attachment_id"],"name":turn.attachment["name"],"source_sha256":turn.attachment["source_sha256"],"media_type":turn.attachment["media_type"],"truncated":turn.attachment["truncated"]}},"model":turn.connection.public()}),
                );
            }
        }
    }
}
