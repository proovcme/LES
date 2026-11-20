//! One turn implementation for JSON and SSE. There is no legacy callback or fallback.
use super::{
    evidence::Evidence,
    journal::Journal,
    model, prepare,
    protocol::{bound_pair, complete_pending, effective, proposal, strip_bound_calls},
    tools, Context, Result, Sink,
};
use les_core::coordinator::{Coordinator, Step};
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
    caller: Option<String>,
) -> Result<Value> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(600);
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
    let mut turn = super::budget::until(
        deadline,
        prepare::prepare(context.clone(), request.clone(), caller, sink),
    )
    .await?;
    let retrieval_mode = if turn.scope["document_grounding_enabled"] == true {
        turn.request["retrieval_mode"].as_str().unwrap_or("pull")
    } else {
        "none"
    }
    .to_owned();
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
    let _permit = tokio::time::timeout(
        Duration::from_secs(30).min(super::budget::remaining(deadline)?),
        slots.acquire_owned(),
    )
    .await
    .map_err(|_| "MODEL_QUEUE_TIMEOUT")?
    .map_err(|_| "MODEL_QUEUE_TIMEOUT")?;
    let mut tools = tools::Tools::new(&context, &turn).await?;
    if turn.egress_tag.is_none() {
        tools.bind_table(super::table_document::TableTool::bind(&context, &turn).await?);
    }
    let available = turn.connection.profile["normal_tool_count"]
        .as_u64()
        .unwrap_or(3)
        > 1 + u64::from(tools.table.is_some())
        && turn.profile["tools"].as_array().is_some_and(|v| {
            v.iter()
                .any(|n| n == "search_sources" || n == "read_source" || n == "document_review")
        });
    if turn.egress_tag.is_none() && retrieval_mode != "push" {
        tools.bind_review(
            super::document_review::ReviewTool::bind(&context, &turn, available).await?,
        );
    }
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
    let direct = retrieval_mode == "push" && tools.table.is_none();
    let mut disclosed = Vec::new();
    let selected_evidence = turn.request["selected_sources_only"] == true
        && turn.scope["document_grounding_enabled"] == true
        && (turn.profile["mode"] == "search" || retrieval_mode != "pull");
    let reading_required = context.index_store.is_some()
        && (selected_evidence || (retrieval_mode == "pull" && turn.profile["mode"] == "search"));
    let mut evidence = Evidence::new(reading_required);
    let mut prefetch = Value::Null;
    let mut candidate_documents = Value::Null;
    let mut named_documents = std::collections::BTreeSet::new();
    if matches!(retrieval_mode.as_str(), "push" | "hybrid") {
        // Hybrid retains the same initial evidence breadth as Push. Enabling
        // tools must not silently discard half the candidate documents.
        sink.progress("retrieval", "Ищу в выбранных документах")
            .await?;
        let result = super::budget::until(
            deadline,
            tools::sources::retrieval::initial(context.clone(), &turn),
        )
        .await?;
        let found_documents = result["result"]["candidate_documents"].clone();
        if tools::sources::selected_files(&turn).is_empty() {
            tools.observe_sources(&result);
        }
        evidence.add(&result);
        named_documents = super::inspection::named_documents(
            turn.request["question"].as_str().unwrap_or(""),
            &found_documents,
        );
        candidate_documents = result["result"]["candidate_documents"].clone();
        prefetch = json!({"coverage":result["result"]["coverage"],
            "index_revision":result["result"]["index_revision"],"read_receipt":result["result"]["read_receipt"],"candidates":evidence.sources.len(),"selection_policy":result["result"]["selection_policy"],"candidate_documents":found_documents,
            "fusion":result["result"]["fusion"],"fusion_trace":result["result"]["fusion_trace"],"read_failures":result["result"]["read_failures"],
            "addressed_row_count":result["result"]["addressed_row_count"]});
        journal
            .checkpoint(
                "retrieval_prefetch",
                json!(json!({
                    "mode":retrieval_mode,"prefetch":prefetch
                })
                .to_string()),
            )
            .await?;
    }
    let mut contexts = Vec::new();
    let base = les_core::chat_context::execute(
        &json!({"action":"profile_prompt","profile":turn.profile}),
    )?;
    let mut prompt = if direct {
        // Output form is chosen once per round below, after inspecting evidence.
        // Do not require prose here and JSON again in the table terminal contract.
        base.clone()
    } else {
        les_core::chat_context::execute(
            &json!({"action":"tool_profile","profile_prompt":base,"native":native}),
        )?
    };
    if let Some(instructions) = tools
        .contracts
        .iter()
        .find(|c| !direct && c["name"] == "agent_workspace")
        .and_then(|c| c["instructions"].as_str())
    {
        prompt=json!(format!("{}\n\n{instructions}\nПрошлые сообщения не подтверждают выполнение действий в текущем запросе. Выполненное действие подтверждается только успешным результатом инструмента в этом запросе.",prompt.as_str().ok_or("CHAT_CONTEXT_INVALID")?));
    }
    let mut agent = Coordinator::new(
        24,
        super::budget::remaining(deadline)?.as_secs_f64(),
        tools
            .contracts
            .iter()
            .filter(|_| !direct)
            .filter_map(|t| t["name"].as_str().map(str::to_owned)),
    )?;
    let mut native_pending = Value::Null;
    let mut table_result = Value::Null;
    let mut review_result = Value::Null;
    let mut table_proposal = Value::Null;
    let mut review_proposal = Value::Null;
    let mut published_sources = json!([]);
    if direct && evidence.needs_read() {
        journal
            .checkpoint(
                "retrieval_prefetch",
                json!(json!({"source_inspection":evidence.reading_trace(),
            "reason":"rust_context_has_no_verified_text"})
                .to_string()),
            )
            .await?;
        return Err("SOURCE_EVIDENCE_MISSING".into());
    }
    let mut grounding_retry = false;
    let mut citation_retry = false;
    let mut answer_window_retry = false;
    let mut table_rows = super::table_facts::Rows::default();
    loop {
        super::budget::remaining(deadline)?;
        match agent.next().await? {
            Step::Model { final_answer } => {
                let mut exchanges = agent.provider_messages(native);
                if native {
                    strip_bound_calls(&mut exchanges, "table_document");
                    strip_bound_calls(&mut exchanges, "document_review");
                }
                let review_active = if let Some(review) = &tools.review {
                    review.active().await
                } else {
                    false
                };
                let source_reading = tools.table.is_none()
                    && !review_active
                    && super::inspection::source_only(agent.results());
                let numeric_grounded = turn.scope["document_grounding_enabled"] == true
                    && (direct || turn.profile["mode"] != "agent" || source_reading);
                if native {
                    complete_pending(&mut exchanges, &agent.stop_reason());
                }
                // A source-only multi-document Pull can answer after any
                // round. Give each read document a place in that window
                // before filling it with additional units from one read.
                let ranked_reads = if source_reading && final_answer {
                    evidence.answer_blocks(1)
                } else if source_reading && retrieval_mode == "pull" {
                    evidence.answer_blocks(2)
                } else {
                    None
                };
                let (mut initial, mut required) = match ranked_reads {
                    Some(blocks) => (blocks, Vec::new()),
                    None => (evidence.optional_blocks(), evidence.required_blocks()),
                };
                let terminal_sources =
                    (final_answer || direct) && numeric_grounded && source_reading;
                let compact_sources = source_reading
                    && numeric_grounded
                    && turn.profile["mode"] == "search"
                    && evidence.has_reads();
                let hybrid_search = retrieval_mode == "hybrid"
                    && turn.profile["mode"] == "search"
                    && source_reading
                    && tools.table.is_none()
                    && !review_active;
                let mut prefetch_focused = false;
                let repair_focused_citations = citation_retry
                    && !answer_window_retry
                    && contexts
                        .last()
                        .is_some_and(|c: &Value| c["prefetch_complete"] == true);
                if native
                    && hybrid_search
                    && (!final_answer || repair_focused_citations)
                    && exchanges.is_empty()
                    && required.is_empty()
                {
                    if let Some(blocks) = evidence.prefetch_blocks(
                        turn.request["question"].as_str().unwrap_or(""),
                        &initial,
                        &named_documents,
                    ) {
                        initial = blocks;
                        prefetch_focused = true;
                    }
                }
                let mut focused_citations = false;
                if final_answer
                    && answer_window_retry
                    && terminal_sources
                    && required.is_empty()
                    && turn.profile["mode"] == "search"
                {
                    if let Some(focused) = evidence.citation_focus(
                        turn.request["question"].as_str().unwrap_or(""),
                        &tools::sources::selected_files(&turn),
                        &initial,
                    ) {
                        initial = focused;
                        focused_citations = true;
                    }
                }
                if compact_sources {
                    evidence.prioritize_workbook_groups(
                        turn.request["question"].as_str().unwrap_or(""),
                        &mut initial,
                    );
                }
                if terminal_sources || compact_sources {
                    for block in initial.iter_mut().chain(&mut required) {
                        Evidence::terminal_block(block);
                    }
                }
                let proof_blocks = required.len();
                let mut native_groups = Vec::new();
                let mut native_history = Vec::new();
                if native && !exchanges.is_empty() {
                    native_history = super::protocol::exchange_groups(&exchanges);
                    if let Some(current) = native_history.pop() {
                        native_groups.push(current);
                    }
                }
                if final_answer && numeric_grounded && source_reading {
                    // The tool phase is closed. Preserve complete provider pairs
                    // as optional history; they cannot evict factual page bodies.
                    native_history.append(&mut native_groups);
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
                if let Some(review) = tools.review.as_ref().filter(|_| review_active) {
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
                let mut advertised = if final_answer || direct {
                    vec![]
                } else {
                    tools.disclosed_schemas(
                        if native && hybrid_search {
                            // Search and extension access suffice until the
                            // model asks for a specific reader.
                            2 + disclosed.len().min(3)
                        } else {
                            tools.schema_limit(
                                turn.connection.profile["normal_tool_count"]
                                    .as_u64()
                                    .unwrap_or(3) as usize,
                                turn.connection.profile["max_tools"].as_u64().unwrap_or(3) as usize,
                                turn.scope["document_grounding_enabled"] == true
                                    && disclosed.is_empty()
                                    && !review_active
                                    && tools.table.is_none(),
                            )
                        },
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
                let reserve = schema_text.chars().count().div_ceil(2) as u64;
                let profile = &turn.connection.profile;
                let input = profile["input_token_limit"]
                    .as_u64()
                    .ok_or("CONTEXT_REQUIRED_SECTION_OVERFLOW")?
                    .checked_sub(reserve)
                    .ok_or("CONTEXT_REQUIRED_SECTION_OVERFLOW")?;
                let mut question = json!({"question":turn.request["question"],"attachment_context":turn.request["attachment_context"],"output_directive":turn.request["output_directive"],"response_length":turn.request["response_length"],"source_scope":turn.scope,"target_file":turn.request["target_file"],"target_files":turn.request["target_files"]});
                if turn.scope["document_grounding_enabled"] == true {
                    if terminal_sources {
                        question["source_coverage_note"] =
                            json!("Ограничь выводы прочитанными фрагментами.");
                    } else {
                        question["source_coverage_note"] = json!("Поисковые выдержки не означают полный просмотр файлов. Отсутствие факта в короткой выдаче не доказывает его отсутствие в документе. Для такого вывода дочитай нужный раздел либо явно ограничь ответ прочитанными фрагментами.");
                        question["context_window_note"] = json!("Цитируй только фрагменты из текущего окна. Если нужный фрагмент не поместился, повтори чтение. Карточка файла или квитанция вызова не заменяет его текст.");
                    }
                    if !direct && !final_answer {
                        question["reader_contract"] = json!("Оригинал PDF проверяет read_pdf_source: возьми doc_id из результата поиска и страницу из заголовка фрагмента. read_source читает индекс. Ошибка чтения не подтверждает оригинал.");
                    }
                }
                let navigation = if candidate_documents.is_array()
                    && exchanges.is_empty()
                    && !prefetch_focused
                    && !final_answer
                    && !direct
                {
                    vec![super::inspection::navigation(
                        &candidate_documents,
                        turn.connection.profile["input_token_limit"]
                            .as_u64()
                            .unwrap_or(8192)
                            .saturating_sub(
                                turn.connection.profile["generation_reserve_tokens"]
                                    .as_u64()
                                    .unwrap_or(2048),
                            )
                            .saturating_sub(
                                turn.connection.profile["safety_reserve_tokens"]
                                    .as_u64()
                                    .unwrap_or(512),
                            )
                            .min(8000) as usize,
                    )]
                } else {
                    vec![]
                };
                if evidence.needs_read() {
                    super::inspection::question(&mut question, evidence.retry_used());
                }
                if prefetch["coverage"] == "no_matches" {
                    question["source_search_note"] = json!("Первичный поиск в выбранных документах не нашёл подтверждения. Отметь ограничение, если дальнейшее чтение не дало источника.");
                }
                if final_answer {
                    question["final_answer"]=json!("Ответь по прочитанным данным. Новые вызовы инструментов недоступны. Если сведений недостаточно, обозначь это.");
                }
                if citation_retry || prefetch_focused {
                    question["citation_format_required"] = json!("Свяжи каждый подтверждённый факт с видимым источником в формате [Источник N]. Бери номер из заголовка выдержки: номера могут идти с пропусками. Если подтверждённых фактов нет, начни ответ с [[NO_EVIDENCE]], причину сформулируй своими словами и не перечисляй ссылки.");
                }
                // Search may answer from a verified prefetch while keeping
                // refinement tools available. Do not force a redundant action
                // on every factual question; Agent retains its action contract.
                let round_base = if (final_answer && numeric_grounded)
                    || (turn.profile["mode"] == "search" && evidence.has_reads())
                {
                    &base
                } else {
                    &prompt
                };
                let round_prompt = if evidence.needs_read() && !final_answer {
                    json!(super::inspection::prompt(
                        round_base.as_str().ok_or("CHAT_CONTEXT_INVALID")?
                    ))
                } else if turn.scope["document_grounding_enabled"] == true {
                    json!(super::inspection::answer_prompt(
                        round_base.as_str().ok_or("CHAT_CONTEXT_INVALID")?,
                        native && !advertised.is_empty()
                    ))
                } else {
                    round_base.clone()
                };
                if numeric_grounded && super::table_facts::Rows::supported(&initial) {
                    question["exact_sum"] = json!(super::table_facts::SUM_CONTRACT);
                }
                let mut context_request = json!({"action":"assemble","preset":{"preset_id":profile["preset_id"],"input_limit":input,"generation_reserve":profile["generation_reserve_tokens"],"safety_reserve":profile["safety_reserve_tokens"]},
                    "profile_prefix":round_prompt,"request_payload":question,"shortlist":if native{json!([])}else{json!(advertised)},
                    "checkpoint":navigation,"working_memory":turn.memory,"dialogue":turn.dialogue,"evidence":initial,"source_map":evidence.sources,"compact_source_evidence":compact_sources,
                    "required_evidence":required,"tool_exchange":if native{json!([])}else{json!(exchanges)},"native_tool_exchange":native_groups,"native_tool_history":native_history});
                let capacity = super::budget::reader_capacity(
                    &context_request,
                    proof_blocks,
                    usize::from(native && !exchanges.is_empty()),
                );
                let packed = match capacity {
                    Ok(chars) => {
                        tools.read_max_chars = chars;
                        context_request["request_payload"]["reader_budget_max_chars"] =
                            json!(chars);
                        les_core::chat_context::execute(&context_request)?
                    }
                    // The capacity probe uses the same packer. Its overflow
                    // must take the same schema-release path as the full call,
                    // rather than aborting before that recovery can run.
                    Err(error) if error.code == "CONTEXT_REQUIRED_SECTION_OVERFLOW" => {
                        json!({"overflow":true})
                    }
                    Err(error) => return Err(error),
                };
                if packed["overflow"] == true {
                    if native
                        && !final_answer
                        && !answer_window_retry
                        && reserve > 0
                        && source_reading
                        && evidence.has_reads()
                        && tools.table.is_none()
                        && !review_active
                    {
                        // The actual continuation, including its tool pair,
                        // did not fit. Reassemble without tool schemas before
                        // calling the provider; never truncate admitted proofs.
                        agent.prepare_answer_window()?;
                        answer_window_retry = true;
                        citation_retry = true;
                        journal.checkpoint("retrieval_prefetch",json!(json!({"answer_window_retry":true,
                            "answer_window_reason":"mandatory_continuation_overflow","schema_reserved_tokens":reserve}).to_string())).await?;
                        journal.checkpoint("reset", json!("")).await?;
                        sink.emit("reset", json!("")).await?;
                        sink.progress(
                            "verification",
                            "Подготавливаю ответ по прочитанным источникам",
                        )
                        .await?;
                        continue;
                    }
                    eprintln!(
                        "{}",
                        json!({"event":"chat_context_overflow","input_limit":input,
                        "schema_reserved_tokens":reserve,"generation_reserve":profile["generation_reserve_tokens"],
                        "required_chars":json!(required).to_string().chars().count(),"exchange_chars":json!(native_groups).to_string().chars().count(),
                        "prompt_chars":prompt.as_str().map(str::len),"required_blocks":required.len()})
                    );
                    return Err("CONTEXT_REQUIRED_SECTION_OVERFLOW".into());
                }
                let visible = les_core::chat_context::execute(
                    &json!({"action":"visible","packet":packed["packet"],"source_map":evidence.sources}),
                )?;
                let missing_named_document = hybrid_search
                    && named_documents.iter().any(|id| {
                        !visible
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|s| s["doc_id"].as_str() == Some(id.as_str()))
                    });
                let prefetch_complete = prefetch_focused
                    && !missing_named_document
                    && initial.iter().all(|block| {
                        visible
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|source| source["index"] == block["source_index"])
                    });
                table_rows = if numeric_grounded {
                    super::table_facts::Rows::new(&packed["packet"], &visible)
                } else {
                    super::table_facts::Rows::default()
                };
                let trace = les_core::chat_context::execute(
                    &json!({"action":"prepared_trace","packet":packed["packet"],"purpose":"native_chat","source_map":evidence.sources,
                        "evidence":initial.iter().chain(required.iter().take(proof_blocks)).collect::<Vec<_>>()}),
                )?;
                if visible != published_sources {
                    journal
                        .checkpoint("sources", json!(visible.to_string()))
                        .await?;
                    published_sources = visible.clone();
                }
                contexts.push(json!({"delivery":"prepared","context":trace,"sources":visible,"schema_reserved_tokens":reserve,"focused_citations":focused_citations,"prefetch_complete":prefetch_complete,"missing_named_document":missing_named_document}));
                sink.emit("sources", json!({"source_map":visible,"sources":visible}))
                    .await?;
                sink.progress("generation", "Модель читает контекст и отвечает")
                    .await?;
                let packet = &packed["packet"];
                let available_output = packet["generation_reserve_tokens"]
                    .as_u64()
                    .ok_or("CHAT_CONTEXT_INVALID")?
                    + packet["input_budget_tokens"]
                        .as_u64()
                        .ok_or("CHAT_CONTEXT_INVALID")?
                        .saturating_sub(
                            packet["included_tokens"]
                                .as_u64()
                                .ok_or("CHAT_CONTEXT_INVALID")?,
                        );
                let mut response = model::complete(
                    &context,
                    &turn,
                    packed["messages"].clone(),
                    if native { advertised } else { vec![] },
                    available_output,
                    journal,
                    sink,
                    turn.scope["document_grounding_enabled"] != true,
                    Value::Null,
                    deadline,
                )
                .await?;
                if let Some(review) = &tools.review {
                    review.acknowledge(packed["messages"].clone()).await?;
                    agent.capability_revision("document_review", review.revision().await)?;
                }
                if !native && !final_answer && !direct {
                    response["selection_text"] = response["text"].clone();
                }
                if let Some(last) = contexts.last_mut() {
                    last["delivery"] = json!("confirmed");
                    last["usage"] = response["usage"].clone();
                    last["context"] = les_core::chat_context::execute(
                        &json!({"action":"confirm","trace":last["context"],"usage":response["usage"]}),
                    )?;
                }
                evidence.confirm_delivery(&visible);
                if response["tool_calls"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                {
                    let calls = response["tool_calls"].clone();
                    native_pending = calls;
                } else {
                    native_pending = Value::Null;
                }
                if native
                    && reading_required
                    && evidence.sources.is_empty()
                    && visible.as_array().is_none_or(Vec::is_empty)
                    && response["tool_calls"].as_array().is_none_or(Vec::is_empty)
                    && response["text"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty())
                    && !grounding_retry
                    && !final_answer
                {
                    grounding_retry = true;
                    sink.progress("retrieval", "Проверяю выбранные источники перед ответом")
                        .await?;
                    let result = super::budget::until(
                        deadline,
                        tools::sources::retrieval::initial(context.clone(), &turn),
                    )
                    .await?;
                    evidence.add(&result);
                    candidate_documents = result["result"]["candidate_documents"].clone();
                    if tools::sources::selected_files(&turn).is_empty() {
                        tools.observe_sources(&result);
                    }
                    journal.checkpoint("retrieval_prefetch", json!(json!({"grounding_retry":{"reason":"model_completed_without_source_read","sources":evidence.sources.len()}}).to_string())).await?;
                    if evidence.sources.is_empty() {
                        return Err("SOURCE_EVIDENCE_MISSING".into());
                    }
                    journal.checkpoint("reset", json!("")).await?;
                    sink.emit("reset", json!("")).await?;
                    // Discard the ungrounded draft. The bounded agent loop asks
                    // the same model again with actual source evidence.
                    agent.retry_model("grounding_evidence_added")?;
                    continue;
                }
                if evidence.needs_read()
                    && !final_answer
                    && response["tool_calls"].as_array().is_none_or(Vec::is_empty)
                    && response["text"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty())
                {
                    journal.checkpoint("reset", json!("")).await?;
                    sink.emit("reset", json!("")).await?;
                    if !evidence.retry_read_once() {
                        return Err("SOURCE_READ_REQUIRED".into());
                    }
                    journal.checkpoint("retrieval_prefetch",json!(json!({"source_inspection":{"status":"required","reason":"model_completed_without_document_read"}}).to_string())).await?;
                    agent.retry_model("source_inspection_required")?;
                    continue;
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
                    .run(context.clone(), &turn, &call, &journal.id, deadline)
                    .await?;
                journal.tool_result(payload.clone()).await?;
                if let Some(name) = tools.disclosed(&payload) {
                    disclosed.retain(|n| n != &name);
                    disclosed.push(name);
                    if disclosed.len() > 128 {
                        disclosed.remove(0);
                    }
                }
                for key in ["_pull_workbook_evidence", "_pull_evidence"] {
                    if payload[key].is_object() {
                        evidence.add(&payload[key]);
                    }
                }
                evidence.add(&payload);
                let mut projected = evidence.projection(&payload, tools.read_max_chars);
                if turn.profile["mode"] == "search" && evidence.has_reads() {
                    super::protocol::compact_source_receipt(&mut projected);
                }
                let effect = effective(&payload);
                if tools::sources::selected_files(&turn).is_empty() {
                    tools.observe_sources(&payload);
                }
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
                }
                sink.emit("tool_result",json!({"tool":call["tool"],"status":payload["status"],"code":payload["result"]["code"]})).await?;
                agent.tool_result_projected(payload, projected)?;
            }
            Step::Failed(code) => return Err(code.into()),
            Step::Finished => return Err("RHO_COMPLETION_NOT_FINAL".into()),
            Step::Complete(terminal) => {
                if matches!(
                    terminal["finish_reason"].as_str(),
                    Some("length" | "max_tokens")
                ) {
                    return Err("MODEL_OUTPUT_LIMIT_REACHED".into());
                }
                let mut visible = contexts
                    .last()
                    .map(|c| c["sources"].clone())
                    .unwrap_or(json!([]));
                let review_active = if let Some(review) = &tools.review {
                    review.active().await
                } else {
                    false
                };
                let omitted_reads = evidence.omitted_explicit_reads(&visible);
                let focused_answer = contexts
                    .last()
                    .is_some_and(|c| c["prefetch_complete"] == true)
                    && !super::inspection::answer_text(
                        terminal["text"].as_str().unwrap_or(""),
                        true,
                    )
                    .1;
                // Deliberately sending a preview is distinct from losing an
                // admitted reading to the tool schemas during packing.
                let read_window = !focused_answer && evidence.read_omitted(&visible);
                let omitted_document = omitted_reads.iter().any(|read| {
                    read["doc_id"].as_str().is_some_and(|doc_id| {
                        !visible
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|source| source["doc_id"] == doc_id)
                    })
                });
                if !citation_retry
                    && !answer_window_retry
                    && tools.table.is_none()
                    && !review_active
                    && super::inspection::source_only(agent.results())
                    && contexts.last().is_some_and(|c| {
                        c["schema_reserved_tokens"].as_u64().is_some_and(|n| n > 1)
                    })
                    && (read_window
                        || omitted_document
                        || contexts
                            .last()
                            .is_some_and(|c| c["missing_named_document"] == true))
                {
                    answer_window_retry = true;
                    journal
                        .checkpoint(
                            "retrieval_prefetch",
                            json!(json!({"answer_window_retry":true}).to_string()),
                        )
                        .await?;
                    journal.checkpoint("reset", json!("")).await?;
                    sink.emit("reset", json!("")).await?;
                    sink.progress(
                        "verification",
                        "Освобождаю место для прочитанных источников",
                    )
                    .await?;
                    agent.prepare_answer_window()?;
                    continue;
                }
                if !evidence.can_answer(&visible) {
                    journal.checkpoint("reset", json!("")).await?;
                    journal
                        .checkpoint(
                            "retrieval_prefetch",
                            json!(json!({"source_inspection":evidence.reading_trace(),
                        "reason":"read_evidence_not_delivered_in_final_context"})
                            .to_string()),
                        )
                        .await?;
                    sink.emit("reset", json!("")).await?;
                    return Err("SOURCE_READ_REQUIRED".into());
                }
                if selected_evidence && visible.as_array().is_none_or(Vec::is_empty) {
                    return Err("SOURCE_EVIDENCE_MISSING".into());
                }
                if visible.as_array().is_some_and(|v| {
                    v.iter().any(|s| {
                        matches!(
                            s["evidence_kind"].as_str(),
                            Some(
                                "physical_pdf_page"
                                    | "physical_pdf_unit"
                                    | "physical_word_section"
                                    | "physical_workbook_row"
                            )
                        )
                    })
                }) {
                    sink.progress("verification", "Проверяю источники ответа")
                        .await?;
                    tools::sources::verify_physical(context.clone(), &visible).await?;
                }
                let (text, unconfirmed) = super::inspection::answer_text(
                    terminal["text"].as_str().unwrap_or(""),
                    turn.scope["document_grounding_enabled"] == true,
                );
                let text = text.as_ref();
                let calculation = if table_rows.present() {
                    table_rows.finish_sum(text)?
                } else {
                    None
                };
                let mut table_facts = Value::Null;
                let text = if let Some((rendered, receipt)) = &calculation {
                    super::table_facts::quote_sources(&mut visible, receipt);
                    table_facts = receipt.clone();
                    rendered.as_str()
                } else {
                    text
                };
                let citations = les_core::citations::execute(
                    &json!({"answer":text,"sources":visible,
                    "merge_adjacent_references":turn.profile["mode"] == "search" && table_facts.is_null()}),
                )?;
                let has_citations = citations["indexes"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty());
                let unresolved = citations["unknown_indexes"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty());
                let must_cite = selected_evidence
                    || (turn.scope["document_grounding_enabled"] == true
                        && super::inspection::source_only(agent.results())
                        && visible
                            .as_array()
                            .is_some_and(|sources| !sources.is_empty()));
                if (unconfirmed && has_citations)
                    || (turn.scope["document_grounding_enabled"] == true && unresolved)
                    || (must_cite && !unconfirmed && !has_citations)
                {
                    if citation_retry {
                        journal.checkpoint("reset", json!("")).await?;
                        sink.emit("reset", json!("")).await?;
                        return Err("SOURCE_CITATION_MISSING".into());
                    }
                    citation_retry = true;
                    sink.progress("verification", "Уточняю ссылки на источники")
                        .await?;
                    journal
                        .checkpoint(
                            "retrieval_prefetch",
                            json!(json!({"citation_retry":true}).to_string()),
                        )
                        .await?;
                    journal.checkpoint("reset", json!("")).await?;
                    sink.emit("reset", json!("")).await?;
                    agent.revise_completion()?;
                    continue;
                }
                let answer = citations["answer"].as_str().unwrap_or(text);
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
                if turn.scope["document_grounding_enabled"] == true {
                    journal.checkpoint("token", json!(answer)).await?;
                    sink.emit("token", json!(answer)).await?;
                }
                let completed = json!({"answer":answer,"response":answer,"sources":names,"source_map":visible,"citation_map":visible,
                    "history_id":journal.history,"latency_sec":started.elapsed().as_secs_f64(),"tokens":terminal["usage"]["completion_tokens"].as_u64().unwrap_or(0),"crag_status":"UNVALIDATED","success":true,
                    "source_scope":{"resolved":turn.scope["resolved_dataset_ids"],"used":visible.as_array().into_iter().flatten().map(|s|s["dataset_id"].clone()).collect::<Vec<_>>()},
                    "query_route":{"channel":"generic","reason":"native_chat"},"retrieval_trace":{"engine":"rust.chat","status":if evidence.read_failures.is_empty(){"ok"}else{"degraded"},"read_failures":evidence.read_failures,"omitted_reads":omitted_reads,"rounds":agent.rounds,"mode":retrieval_mode,"prefetch":prefetch,"contexts":contexts,"citation_syntax":citations,"grounding_retry":grounding_retry,"citation_retry":citation_retry,"answer_window_retry":answer_window_retry,"source_inspection":evidence.reading_trace()},
                    "artifact":{"completion_status":if !summary.is_null()&&summary["status"]!="recorded"{"incomplete"}else{"completed"},"execution_receipt":receipt,"execution_summary":summary,"validation":if table_facts.is_null(){"not_performed"}else if table_facts["status"]=="insufficient"{"insufficient_evidence"}else{"source_rows_verified"},"table_facts":table_facts,"tool_results":agent.results(),"egress_tag":turn.egress_tag,"attachment":{
                        "attachment_id":turn.attachment["attachment_id"],"name":turn.attachment["name"],"source_sha256":turn.attachment["source_sha256"],"media_type":turn.attachment["media_type"],"truncated":turn.attachment["truncated"]}},"model":turn.connection.public()});
                agent.accept_completed(json!({"text":answer})).await?;
                return Ok(completed);
            }
        }
    }
}
