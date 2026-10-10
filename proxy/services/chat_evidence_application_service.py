"""General evidence/RAG application execution extracted from the HTTP router.

The caller resolves request scope and deterministic tools first. This service owns the
unchanged retrieval -> context/evidence -> model -> sources/trace execution branch.
"""
from __future__ import annotations
from proxy.services.operation_progress_service import chat_progress
from proxy.services.chat_inference_service import resolve_required_answer, run_bound_inference

import asyncio
import hashlib
import json
import logging
import os
import time
from dataclasses import dataclass, field
from pathlib import Path
from backend.runtime_paths import mutable_path
from typing import Any, Callable, Mapping, Sequence

import httpx
from fastapi import HTTPException

from backend.inference.routing import cloud_allowed
from proxy.services.answer_form_service import classify_answer_form
from proxy.services.answer_form_service import apply_response_length
from proxy.services.cad_bim_highlight import extract_highlight, set_highlight
from proxy.services.canonical_route_service import (
    BoundModelChatRunner,
    CanonicalRouteMode,
    canonical_route_trace_payload,
    one_model_decision_from_calls,
    resolve_canonical_route,
)
from proxy.services.model_connection_contracts import ConnectionLocality, ConnectionRole
from proxy.services.openai_compatible_transport_service import InferenceRequest, ModelTransportError
from proxy.services.context_governor_service import (
    ContextCandidate,
    ContextGovernor,
    ContextKind,
    ContextObject,
    ContextPacket,
    ContextRequiredSectionOverflow,
)
from proxy.services.chat_section_context_service import ChatSectionReader, SectionReadError, visible_chunks
from proxy.services.evidence_packet_service import (
    build_retrieval_evidence_packet,
    render_retrieval_evidence_for_model,
)
from proxy.services.lexical_index_service import retrieval_fingerprint
from proxy.services.notebook_service import dataset_memory_prompt_excerpt
from proxy.services.notebook_study_service import is_notebook_study_query
from proxy.services.project_summary_service import (
    build_project_summary,
    format_project_inventory_context,
    format_project_inventory_prompt,
)
from proxy.services.prompt_registry_service import build_mode_system_prompt
from proxy.services.chat_profile_service import effective_retrieval_policy
from proxy.services.retrieval_service import required_reranker_policy, retrieve_chat_chunks
from proxy.services.generation_guard_service import generation_guard
from proxy.services.runtime_admission import GenerationSlotTimeout
from proxy.services.saferag_service import concentrate_sources, rank_chunks_for_question, source_names
from proxy.services.memory_port import get_memory_port


from proxy.services.model_execution_preset_service import ModelExecutionPreset
from proxy.services.model_research_tool_service import (
    ModelResearchToolService,
)
from proxy.services.source_locator_service import evidence_counts, source_map_item
from proxy.services.chat_evidence_manifest_service import build_evidence_manifest
from proxy.services.chat_capability_scope_service import filter_profile_tools
from proxy.services.typed_memory_projection_service import (
    MemoryLimits,
    project_memory,
    resolve_session_memory_scope,
)
from proxy.services.web_research_config_service import (
    WebResearchConfig,
    capture_web_research_config,
    public_web_research_config,
)

from proxy.services.chat_evidence_tools import tool_selector_request_payload
from proxy.services.chat_evidence_tools import _WebToolSourceChunk
from proxy.services.chat_evidence_tools import web_tools_for_request
from proxy.services.chat_evidence_tools import web_source_map_from_tool_results
from proxy.services.chat_evidence_tools import _merge_web_source_map
from proxy.services.chat_evidence_tools import tools_for_document_scope
from proxy.services.chat_evidence_tools import native_model_tool_schemas
from proxy.services.chat_evidence_contracts import _SkippedRetrievalQuality
from proxy.services.chat_evidence_contracts import _SkippedRetrievalTrace
from proxy.services.chat_evidence_contracts import _SkippedDocumentRetrieval
from proxy.services.chat_evidence_tools import tool_call_identity
from proxy.services.chat_evidence_tools import safe_selected_call_trace
from proxy.services.chat_evidence_context import _context_objects
from proxy.services.chat_evidence_context import _text_context_objects
from proxy.services.chat_evidence_context import workspace_memory_objects
from proxy.services.chat_evidence_context import govern_inference_messages, source_context_blocks, model_visible_source_map
from proxy.services.chat_evidence_context import context_packet_trace
from proxy.services.chat_evidence_tools import execute_canonical_shadow_decision
from proxy.services.chat_evidence_tools import safe_execute_canonical_shadow_decision
from proxy.services.chat_evidence_context import profile_temperature
from proxy.services.chat_evidence_context import profile_research_rounds
from proxy.services.chat_evidence_context import _bounded_source_blocks
from proxy.services.chat_evidence_context import selector_evidence_payload
from proxy.services.chat_evidence_context import selector_context_shortlist
from proxy.services.chat_evidence_context import initial_selector_context
from proxy.services.chat_evidence_context import profile_system_prompt
from proxy.services.chat_evidence_context import profile_tool_selector_prompt
from proxy.services.chat_evidence_contracts import EvidenceRequestContext
from proxy.services.chat_evidence_contracts import EvidenceRuntimeDeps
from proxy.services.chat_evidence_contracts import ResponseBoundary

logger = logging.getLogger(__name__)

_DOCUMENT_EVIDENCE_TOOLS = frozenset({
    "dataset_map",
    "search_sources",
    "read_source",
    "read_pdf_source",
    "look_at_pdf_page",
    "read_excel_source",
    "search_project_tables",
    "read_project_table",
    "assemble_project_volume",
})


async def run_chat_evidence_application(
    request: EvidenceRequestContext, runtime: EvidenceRuntimeDeps, response: ResponseBoundary,
):
    return await _execute_chat_evidence_application(request, runtime, response)


async def _execute_chat_evidence_application(
    request: EvidenceRequestContext, runtime: EvidenceRuntimeDeps, response: ResponseBoundary,
):
    _dataset_ids = request.dataset_ids
    scope_resolution = request.scope_resolution
    class_suggestions = request.class_suggestions
    dataset_name_by_id = request.dataset_name_by_id
    effective_dataset_filter = request.effective_dataset_filter
    inventory_requested = request.inventory_requested
    memory_block = request.memory_block
    query_route_payload = request.query_route_payload
    req = request.req
    resolved_dataset_names = request.resolved_dataset_names
    route = request.route
    session_block = request.session_block
    study_requested = request.study_requested
    t_request_start = request.request_started_at
    table_result = request.table_result
    target_doc_filter = request.target_doc_filter
    target_file_ref = request.target_file_ref
    topic_doc_filter = request.topic_doc_filter
    topic_retrieval_plan = request.topic_retrieval_plan
    use_semantic_cache = request.use_semantic_cache
    use_validation = request.use_validation
    validation_skip_reason = request.validation_skip_reason
    profile_snapshot = request.profile_snapshot
    state = runtime.state
    rag_backend = runtime.rag_backend
    cache = runtime.cache
    cache_embedding = runtime.cache_embedding
    cache_marker = runtime.cache_marker
    cache_scope = runtime.cache_scope
    _augment_model_tool_args = runtime.augment_model_tool_args
    _compact_tool_result_for_prompt = runtime.compact_tool_result_for_prompt
    _dataset_ids_from_chunks = runtime.dataset_ids_from_chunks
    _dataset_sensitivities = runtime.dataset_sensitivities
    _env_bool = runtime.env_bool
    _env_float = runtime.env_float
    _env_int = runtime.env_int
    _format_tool_results_for_model = runtime.format_tool_results_for_model
    _generation_token_budget = runtime.generation_token_budget
    _local_context_budget = runtime.local_context_budget
    _names_for_dataset_ids = runtime.names_for_dataset_ids
    _parse_model_tool_calls = runtime.parse_model_tool_calls
    _prepare_notebook_reader_memory = runtime.prepare_notebook_reader_memory
    _record_cloud_cost = runtime.record_cloud_cost
    retrieve_chat_chunks = runtime.retrieve_chat_chunks
    source_excerpts = runtime.source_excerpts
    model_connection_resolver = runtime.model_connection_resolver
    model_connection_transport = runtime.model_connection_transport

    save_chat_history = response.save_chat_history
    token_sink = response.token_sink
    _version_stamp = response.version_stamp
    answer = None
    history_id = None
    key = None
    payload = None
    retrieval = None
    source_dataset_ids = None
    source_dataset_names = None
    sources_list = None
    status = None
    _dataset_ids = tuple(str(item) for item in (_dataset_ids or ()) if str(item))
    document_grounding_enabled = bool(
        (scope_resolution or {}).get("document_grounding_enabled", _dataset_ids)
    )
    if document_grounding_enabled:
        use_semantic_cache = False
    # Ordinary chat is model-owned: validators may not judge, retry, rewrite,
    # suppress, or relabel the model's engineering conclusion.
    use_validation = False
    memory_project_id = 0
    workspace_memory_registered = False
    project_memory_advisory = ""
    try:
        memory_project_id, workspace_memory_registered = resolve_session_memory_scope(
            str(req.session_id or ""), int(getattr(req, "project_id", 0) or 0) or None,
        )
        memory_project_id = int(memory_project_id or 0)
        if memory_project_id > 0 and not workspace_memory_registered:
            project_memory_advisory = get_memory_port().recall_project_advisory(
                memory_project_id, str(req.question or "")
            )
    except Exception as memory_error:  # Memory is advisory and fail-open.
        logger.warning("[MEMORY] advisory recall skipped: %s", memory_error)
        memory_project_id = 0
        project_memory_advisory = ""

    # Карты тем/разделов остаются навигацией для модели. Production chat-path
    # физически не делает topic/file prefetch до первого модельного хода.
    requested_topic_doc_filter = list(topic_doc_filter or [])
    topic_doc_filter = []

    await chat_progress(token_sink, "retrieval", "Ищу в выбранных источниках")
    t_search_start = time.time()
    try:
        _reranker_on, retrieval_trace_policy = required_reranker_policy(
            getattr(req, "reranker_enabled", None)
        )
        topic_chunks: list[Any] = []
        if document_grounding_enabled:
            retrieval = await retrieve_chat_chunks(
                question=req.question,
                dataset_ids=_dataset_ids,
                rag_backend=rag_backend,
                reranker_enabled=_reranker_on,
                reranker_available=state.reranker_available,
                reranker_cls=state.reranker_cls,
                mlx_url=os.getenv("MLX_URL", "http://127.0.0.1:8080"),
                logger=logger,
                llm_semaphore=state.llm_semaphore,
                return_trace=True,
                doc_filter=target_doc_filter or None,
                scope_source=str((scope_resolution or {}).get("scope_source") or "unspecified"),
                scope_error_code=str((scope_resolution or {}).get("error_code") or ""),
            )
        else:
            retrieval = _SkippedDocumentRetrieval()
        chunks = [*topic_chunks, *retrieval.chunks] if topic_chunks else list(retrieval.chunks)
    except Exception as e:
        import traceback

        tb = traceback.format_exc()
        logger.error("[CHAT] RETRIEVAL ERROR: %s\n%s", e, tb)
        raise HTTPException(500, f"Поиск по датасету не удался: {type(e).__name__}: {e}")
    t_search = time.time() - t_search_start
    retrieval_trace = retrieval.payload()
    retrieval_trace["scope_resolution"] = dict(scope_resolution or {})
    retrieval_trace["reranker_policy"] = retrieval_trace_policy
    retrieval_trace_object = getattr(retrieval, "trace", None)
    retrieval_status = str(
        getattr(retrieval_trace_object, "status", "") or retrieval_trace.get("status") or "ok"
    )
    if retrieval_status == "blocked":
        error_code = str(
            getattr(retrieval_trace_object, "error_code", "")
            or retrieval_trace.get("error_code")
            or "retrieval_blocked"
        )
        if error_code in {"dataset_scope_not_found", "no_datasets", "corpus_empty"}:
            blocked_answer = (
                "Нужный набор данных не найден или пока пуст. "
                "Выберите доступный проект/датасет либо добавьте источники — "
                "поиск по другим документам автоматически не выполнялся."
            )
            action = "Выбрать доступный датасет или загрузить источники."
        elif error_code in {"embedding_contract_mismatch", "INDEX_RECOVERY_REQUIRED",
                            "INDEX_RECOVERY_INCOMPLETE", "INDEX_UPDATE_BUSY", "INDEX_CHANGED_DURING_SEARCH",
                            "ROLE_BINDING_MISSING", "CONNECTION_DISABLED", "CONNECTION_SECRET_MISSING",
                            "CAPABILITY_SNAPSHOT_MISSING", "CAPABILITY_SNAPSHOT_STALE", "CAPABILITY_REQUIRED",
                            "UPSTREAM_TIMEOUT", "UPSTREAM_UNREACHABLE", "UPSTREAM_AUTH_FAILED",
                            "UPSTREAM_MODEL_NOT_FOUND", "UPSTREAM_RATE_LIMITED",
                            "UPSTREAM_RESPONSE_INVALID", "UPSTREAM_RESPONSE_TOO_LARGE"}:
            from proxy.services.public_error_service import public_error_payload
            blocked_answer = public_error_payload(status_code=503, detail=error_code)["detail"]
            action = blocked_answer
        elif error_code in {"reranker_disabled", "reranker_unavailable", "reranker_failed"}:
            blocked_answer = (
                "Реранкер не смог обработать найденные источники. "
                "Отключите «Уточнять порядок источников» и повторите вопрос "
                "или проверьте реранкер в диагностике."
            )
            action = "Отключить реранкер или восстановить его, затем повторить вопрос."
        else:
            blocked_answer = (
                "Не удалось выполнить поиск по выбранным документам. "
                "Проверьте готовность набора в разделе «Данные» и повторите вопрос."
            )
            action = "Открыть «Данные» и проверить готовность набора к поиску."
        retrieval_trace["blocker"] = {
            "schema": "retrieval_blocker_v1",
            "code": error_code,
            "action": action,
        }
        state.crag_stats["no_data"] += 1
        state.chat_metrics["retrieval_weak"] = state.chat_metrics.get("retrieval_weak", 0) + 1
        state.chat_metrics["latency_search"].append(t_search)
        state.chat_metrics["latency_gen"].append(0.0)
        state.chat_metrics["tokens"].append(0)
        state.chat_metrics["crag_fail"] += 1
        for key_name in ("latency_search", "latency_gen", "tokens"):
            state.chat_metrics[key_name] = state.chat_metrics[key_name][-100:]
        history_id = None
        try:
            history_id = save_chat_history(
                question=req.question,
                answer=blocked_answer,
                sources=[],
                crag_status="BLOCKED",
                latency_sec=t_search,
                tokens=0,
                session_id=req.session_id,
                requested_dataset_filter=req.dataset_filter,
                effective_dataset_filter=effective_dataset_filter,
                resolved_dataset_ids=_dataset_ids,
                resolved_dataset_names=resolved_dataset_names,
                query_route=query_route_payload,
                retrieval_trace=retrieval_trace,
                cache_type=cache_marker,
                validation_enabled=False,
                success=0,
            )
        except Exception as db_err:
            logger.warning("[CHAT] History save error: %s", db_err)
        return {
            "answer": blocked_answer,
            "crag_status": "BLOCKED",
            "sources": [],
            "effective_dataset_filter": effective_dataset_filter,
            "query_route": query_route_payload,
            "retrieval_trace": retrieval_trace,
            "blocker": retrieval_trace["blocker"],
            "cache": cache_marker,
            "validation": {"enabled": False, "reason": error_code},
            "history_id": history_id,
        }
    if topic_retrieval_plan:
        found_topic_docs = {str(getattr(chunk, "doc_name", "") or "") for chunk in topic_chunks}
        retrieval_trace["topic_guided_retrieval"] = {
            "schema": topic_retrieval_plan.get("schema") or "dataset_topic_selection_v1",
            "context_role": "navigation",
            "is_evidence": False,
            "selected_topics": topic_retrieval_plan.get("selected_topics") or [],
            "selected_files": topic_retrieval_plan.get("selected_files") or [],
            "selected_sections": topic_retrieval_plan.get("selected_sections") or [],
            "requested_doc_filter": requested_topic_doc_filter,
            "targeted_doc_filter": topic_doc_filter,
            "prefetch_enabled": False,
            "targeted_trace": {},
            "targeted_chunk_count": len(topic_chunks),
            "wide_fallback_trace": retrieval.payload(),
            "wide_fallback_chunk_count": len(retrieval.chunks),
            "fallback": topic_retrieval_plan.get("fallback") or "wide_retrieval",
            "not_found_files": [name for name in topic_doc_filter if name not in found_topic_docs],
        }
    if validation_skip_reason:
        retrieval_trace["validation_policy"] = {
            "enabled": False,
            "reason": validation_skip_reason,
            "evidence": "source_map+project_inventory_artifact",
        }
    if target_file_ref:
        retrieval_trace["target_file"] = target_file_ref
    if retrieval.quality.status == "good":
        state.chat_metrics["retrieval_good"] = state.chat_metrics.get("retrieval_good", 0) + 1
    else:
        state.chat_metrics["retrieval_weak"] = state.chat_metrics.get("retrieval_weak", 0) + 1

    notebook_study_pack = None
    notebook_study_prompt = ""
    notebook_study_artifact = ""
    notebook_study_latency = 0.0
    notebook_study_started = time.time()
    dataset_memory_prompt = ""
    project_inventory_prompt = ""
    project_inventory_artifact_text = ""
    project_inventory_payload: dict[str, Any] | None = None
    if _dataset_ids and study_requested:
        try:
            retrieval_trace["dataset_reader_prepare"] = await _prepare_notebook_reader_memory(
                [str(d) for d in _dataset_ids],
            )
        except Exception as reader_err:  # noqa: BLE001
            logger.warning("[DATASET_READER] study prepare failed: %s", reader_err)
            retrieval_trace["dataset_reader_prepare"] = {
                "schema": "dataset_reader_prepare_v1",
                "status": "skipped",
                "error": f"{type(reader_err).__name__}: {reader_err}",
            }
    if _dataset_ids:
        try:
            dataset_memory_prompt = await asyncio.to_thread(
                dataset_memory_prompt_excerpt,
                [str(d) for d in _dataset_ids],
                question=req.question,
            )
            if dataset_memory_prompt:
                retrieval_trace["dataset_memory"] = {
                    "schema": "dataset_brief_for_model_v1",
                    "context_role": "navigation",
                    "is_evidence": False,
                    "dataset_count": len(_dataset_ids),
                    "prompt_chars": len(dataset_memory_prompt),
                }
        except Exception as memory_err:  # noqa: BLE001
            logger.warning("[DATASET_MEMORY] skipped: %s", memory_err)
            retrieval_trace["dataset_memory"] = {
                "schema": "dataset_memory_context_v1",
                "status": "skipped",
                "error": f"{type(memory_err).__name__}: {memory_err}",
            }
    if _dataset_ids and (inventory_requested or study_requested):
        try:
            project_inventory_payload = await asyncio.to_thread(
                build_project_summary,
                [str(d) for d in _dataset_ids],
                storage_root=mutable_path("./storage/datasets"),
            )
            if inventory_requested:
                project_inventory_prompt = format_project_inventory_prompt(
                    project_inventory_payload,
                    label=", ".join(resolved_dataset_names or [str(d) for d in _dataset_ids]),
                )
                project_inventory_artifact_text = format_project_inventory_context(
                    project_inventory_payload,
                    label=", ".join(resolved_dataset_names or [str(d) for d in _dataset_ids]),
                )
            retrieval_trace["project_inventory"] = {
                "schema": "project_inventory_context_v1",
                "context_role": "deterministic_evidence",
                "source": "metadb.documents",
                "file_count": project_inventory_payload.get("file_count", 0),
                "by_ext": (project_inventory_payload.get("inventory") or {}).get("by_ext") or [],
                "prompt_chars": len(project_inventory_prompt),
                "artifact_chars": len(project_inventory_artifact_text),
                "used_for_notebook_study": bool(study_requested),
            }
        except Exception as inv_err:  # noqa: BLE001
            logger.warning("[PROJECT_INVENTORY] skipped: %s", inv_err)
            retrieval_trace["project_inventory"] = {
                "schema": "project_inventory_context_v1",
                "status": "skipped",
                "error": f"{type(inv_err).__name__}: {inv_err}",
            }
    if _dataset_ids and is_notebook_study_query(req.question):
        retrieval_trace["notebook_study"] = {
            "schema": "notebook_study_v1",
            "status": "map_only",
            "query_prefetch_enabled": False,
            "reason": "model_first_single_rrf",
            "note": "dataset map and inventory are navigation; no automatic section/file retrieval",
        }
    notebook_study_latency = time.time() - notebook_study_started
    retrieval_trace["notebook_study_latency_sec"] = round(notebook_study_latency, 3)

    # «Заставь отвечать»: не хард-режем разнородность, если есть сильный сигнал —
    # пользователь задал датасет (уже сузил) ИЛИ топ-совпадение хорошее (есть, что
    # отвечать). Гейт остаётся только для реально широких безскоповых слабых запросов.
    inventory_has_files = bool(project_inventory_payload and int(project_inventory_payload.get("file_count") or 0) > 0)
    strong_signal = bool(effective_dataset_filter) or inventory_has_files or (retrieval.quality.top_score >= 0.5)
    if retrieval.quality.status == "needs_clarification" and not strong_signal:
        retrieval_trace["wide_scope"] = {"model_final_allowed": True, "reason": "low_concentration"}

    is_structured = any(word in req.question.casefold() for word in ("перечен", "состав", "список", "разделы", "все разделы", "перечисли"))
    is_technical_or_legal = bool(effective_dataset_filter and effective_dataset_filter != "MAIL")

    # LES uses the explicitly assigned answer connection for every request.
    # Promotion/shadow experiments and implicit provider fallbacks are not a product path.
    try:
        connection_resolver, connection_secret_store, resolved_connection = resolve_required_answer(model_connection_resolver)
    except Exception as error:
        raise HTTPException(503, str(error)) from error
    from proxy.services.table_document_tool import (
        bind_table_tool, manifest as table_manifest, TOOL_NAME as TABLE_TOOL,
        without_table_replays,
    )
    table_tool = await asyncio.to_thread(
        bind_table_tool, req, dataset_ids=_dataset_ids,
        profile_revision=str((profile_snapshot or {}).get("revision_id") or ""),
        model_revision=resolved_connection.revision_id,
        input_budget=resolved_connection.effective_preset.input_token_limit,
    )
    if table_tool is not None:
        use_semantic_cache = False
        retrieval_trace["document_task"] = table_tool.state()
    canonical_route = resolve_canonical_route(receipt=None)
    canonical_execution_mode = CanonicalRouteMode.ACTIVE
    candidate_acceptance = bool(getattr(req, "candidate_acceptance", False))
    if candidate_acceptance:
        retrieval_trace["candidate_acceptance"] = {
            "enabled": True, "execution_mode": "active",
            "promotion_receipt": "not_used", "state_root": "process_cwd_isolated",
        }

    will_be_cloud = resolved_connection.locality is ConnectionLocality.REMOTE
    big_context = (is_structured or is_technical_or_legal) and will_be_cloud
    local_big = (is_structured or is_technical_or_legal) and not will_be_cloud

    context_budget = _local_context_budget(
        local_big=local_big,
        big_context=big_context,
        provider="model_connection",
    )
    focus_max_chunks = context_budget["focus_max_chunks"] or None
    context_max_chunks = context_budget["context_max_chunks"] or None
    context_chars_limit = context_budget["context_chars_limit"]
    context_window_chars = context_budget["context_window_chars"]
    context_radius = 0 if is_structured else None

    chunks = rank_chunks_for_question(req.question, chunks, preserve_retrieval_order=True)
    protected_doc_names: list[str] = list(target_doc_filter or [])
    protected_doc_names.extend(topic_doc_filter)
    if notebook_study_pack is not None:
        protected_doc_names.extend([
            str(item.get("file_name") or "")
            for item in getattr(notebook_study_pack, "targeted_files", [])
            if item.get("file_name")
        ])
    protected_doc_names = list(dict.fromkeys(name for name in protected_doc_names if name))
    focus_max_docs = max(1, len({str(getattr(chunk, "doc_name", "") or "") for chunk in chunks}))
    chunks = concentrate_sources(
        chunks,
        max_docs=focus_max_docs,
        min_score=float("-inf"),
        max_chunks=focus_max_chunks,
        protected_doc_names=protected_doc_names,
    )
    if topic_doc_filter and retrieval.chunks:
        topic_names = {str(name or "") for name in topic_doc_filter}
        topic_basenames = {Path(name).name for name in topic_names}
        focused_names = {str(getattr(chunk, "doc_name", "") or "") for chunk in chunks}
        fallback_floor = _env_float("RAG_CHAT_FOCUS_MIN_SCORE", 0.35)
        promoted_fallback = None
        for candidate in rank_chunks_for_question(req.question, list(retrieval.chunks), preserve_retrieval_order=True):
            candidate_name = str(getattr(candidate, "doc_name", "") or "")
            if (
                not candidate_name
                or candidate_name in topic_names
                or candidate_name in focused_names
                or Path(candidate_name).name in topic_basenames
            ):
                continue
            candidate_score = float(getattr(candidate, "_rank_score", getattr(candidate, "score", 0.0)) or 0.0)
            if candidate_score < fallback_floor:
                continue
            insert_at = min(len(chunks), 5)
            if focus_max_chunks is not None and len(chunks) >= focus_max_chunks:
                chunks = [*chunks[:insert_at], candidate, *chunks[insert_at: max(focus_max_chunks - 1, insert_at)]]
            else:
                chunks = [*chunks[:insert_at], candidate, *chunks[insert_at:]]
            promoted_fallback = {
                "doc_name": candidate_name,
                "rank_score": round(candidate_score, 4),
            }
            break
        if promoted_fallback:
            retrieval_trace.setdefault("topic_guided_retrieval", {})["wide_fallback_promoted"] = promoted_fallback
    if protected_doc_names:
        retrieval_trace.setdefault("notebook_study", {})["protected_doc_names"] = protected_doc_names
    logger.info(
        "[FOCUS] После концентрации: %s чанков из %s источников",
        len(chunks),
        len(set(c.doc_name for c in chunks)),
    )
    focused_fingerprint = retrieval_fingerprint(chunks)

    if use_semantic_cache and cache_scope and not use_validation:
        session_hit = cache.lookup_session_unvalidated(
            req.question,
            cache_scope,
            focused_fingerprint,
            req.session_id,
        )
        if session_hit:
            state.chat_metrics["cache_hit"] = state.chat_metrics.get("cache_hit", 0) + 1
            history_id = None
            try:
                history_id = save_chat_history(
                    question=req.question,
                    answer=session_hit.answer,
                    sources=session_hit.sources,
                    crag_status="UNVALIDATED",
                    latency_sec=t_search,
                    tokens=0,
                    session_id=req.session_id,
                    requested_dataset_filter=req.dataset_filter,
                    effective_dataset_filter=effective_dataset_filter,
                    resolved_dataset_ids=_dataset_ids,
                    resolved_dataset_names=resolved_dataset_names,
                    source_dataset_ids=_dataset_ids,
                    source_dataset_names=resolved_dataset_names,
                    query_route=query_route_payload,
                    retrieval_trace=retrieval_trace,
                    cache_type=session_hit.cache_type,
                    validation_enabled=use_validation,
                    success=1,
                )
            except Exception as db_err:
                logger.warning("[CHAT] History save error: %s", db_err)
            return {
                "answer": session_hit.answer,
                "crag_status": "UNVALIDATED",
                "sources": session_hit.sources,
                "effective_dataset_filter": effective_dataset_filter,
                "query_route": query_route_payload,
                "retrieval_trace": retrieval_trace,
                "cache": session_hit.cache_type,
                "validation": {"enabled": use_validation},
                "history_id": history_id,
            }
    state.chat_metrics["cache_miss"] = state.chat_metrics.get("cache_miss", 0) + 1

    if not chunks:
        retrieval_trace["empty_retrieval"] = {
            "schema": "empty_retrieval_model_first_v1",
            "model_final_allowed": True,
            "note": "No retrieved chunks; continue to model with memory/navigation instead of code NO_DATA final.",
        }

    t_ctx_start = time.time()
    section_reader = ChatSectionReader(rag_backend, _dataset_ids)

    async def expand_chat_context(values, **kwargs):
        if values:
            await chat_progress(token_sink, "context", "Дочитываю найденные разделы")
        expanded = await section_reader.expand(values, **kwargs)
        if values:
            await chat_progress(token_sink, "context", "Собираю контекст с отдельной ссылкой на каждый фрагмент")
        return expanded

    context_windows = await expand_chat_context(
        chunks,
        collection=getattr(rag_backend, "collection_name", ""),
        logger=logger,
        max_chunks=context_max_chunks,
        max_chars_per_chunk=context_window_chars,
        radius=context_radius,
    )
    llm_chunks = context_windows.chunks
    retrieval_trace["context_window"] = context_windows.payload()
    retrieval_trace["context_budget"] = {
        **context_budget,
        "big_context": big_context,
        "local_big": local_big,
        "will_be_cloud": will_be_cloud,
        "context_radius": context_radius,
    }
    # ПЕРФ: валидатор теперь аддитивный/быстрый (rules+coreml fail-open) — ему НЕ нужен второй
    # дорогой проход expand_context_windows (это удваивало context-фазу, 2.7-5.7с на сложных).
    # Переиспользуем контекст ответа: те же чанки, валидатор проверяет ответ по ним.
    # Отдельный проход вернуть: RAG_VALIDATION_SEPARATE_CONTEXT=true.
    if _env_bool("RAG_VALIDATION_SEPARATE_CONTEXT", False):
        validation_context_windows = await expand_chat_context(
            chunks,
            collection=getattr(rag_backend, "collection_name", ""),
            logger=logger,
            max_chunks=_env_int("RAG_VALIDATION_CONTEXT_MAX_CHUNKS", 10),
            max_chars_per_chunk=_env_int("RAG_VALIDATION_CONTEXT_WINDOW_CHARS", 2600),
            radius=_env_int("RAG_VALIDATION_CONTEXT_RADIUS", 1),
        )
    else:
        validation_context_windows = context_windows
    retrieval_trace["validation_context_window"] = validation_context_windows.payload()
    t_ctx = time.time() - t_ctx_start

    from proxy.services.model_reasoning_service import profile_execution_preset
    try:
        execution_preset = profile_execution_preset(
            resolved_connection.effective_preset, (profile_snapshot or {}).get("model_policy") or {})
    except ValueError as error:
        raise HTTPException(422, str(error)) from error
    preset_diagnostics = execution_preset.diagnostics()
    preset_diagnostics["model_preset"]["requested"] = resolved_connection.model_id
    retrieval_trace["model_execution_profile"] = preset_diagnostics
    retrieval_trace["context_governor"] = {
        "schema": "les.context-governor.v1",
        "preset_id": execution_preset.preset_id,
        "calls": [],
    }
    memory_projection_stage = "project_memory"
    try:
        typed_memory = await asyncio.to_thread(
            project_memory,
            session_id=str(req.session_id or ""),
            project_id=memory_project_id or None,
            dataset_ids=tuple(str(item) for item in _dataset_ids if str(item)),
            limits=MemoryLimits(),
        )
        memory_projection_stage = "context_candidates"
        memory_candidates = typed_memory.as_context_candidates()
        memory_projection_stage = "trace_projection"
        retrieval_trace["typed_memory"] = {
            "schema": "les.typed-memory-projection.v1",
            "context_role": typed_memory.context_role,
            "is_evidence": False,
            "items": len(typed_memory.items),
            "omitted": typed_memory.omitted,
            "cursor": typed_memory.cursor,
            "project_id": typed_memory.project_id,
            "registered_session": workspace_memory_registered,
            "item_ids": [item.item_id for item in typed_memory.items],
            "omitted_item_ids": list(typed_memory.omitted_item_ids),
        }
    except Exception as memory_error:  # noqa: BLE001 - memory is advisory, never an answer blocker
        logger.warning("[TYPED_MEMORY] projection skipped: %s", type(memory_error).__name__)
        memory_candidates = ()
        retrieval_trace["typed_memory"] = {
            "schema": "les.typed-memory-projection.v1",
            "status": "skipped",
            "error_type": type(memory_error).__name__,
            "error_stage": memory_projection_stage,
            "context_role": "advisory_state",
            "is_evidence": False,
        }
    retrieval_trace["routing"] = {
        "configured_provider": "model_connection",
        "configured_model": resolved_connection.model_id,
        "effective_provider": "model_connection",
        "effective_model": resolved_connection.model_id,
        "downgraded": False,
        "is_cloud": will_be_cloud,
    }
    retrieval_trace["model_connection_candidate"] = {
        "revision_id": resolved_connection.revision_id,
        "locality": resolved_connection.locality.value,
        "effective": True,
        "resolution_error": "",
    }
    llm_model = resolved_connection.model_id

    # The central RAG role pack already owns engineering style, source boundaries,
    # navigation-vs-evidence and human-facing wording.  Repeating those rules here
    # used to add thousands of prompt characters and, worse, made the application
    # service a second hidden prompt registry.  Keep only the source-label contract
    # that is specific to the evidence packet rendered below.
    sys_normal = profile_system_prompt(profile_snapshot, strict=False)
    sys_strict = profile_system_prompt(profile_snapshot, strict=True)

    # ADR-12 слой 2: форму ответа диктует интент вопроса (детерминированно, до генерации).
    answer_form = apply_response_length(classify_answer_form(req.question), req.response_length)
    retrieval_trace["answer_form"] = {"intent": answer_form.intent, "max_tokens": answer_form.max_tokens}
    if class_suggestions:
        retrieval_trace["class_suggestions"] = [s["class"] for s in class_suggestions]

    t_gen_start = time.time()
    t_llm = 0.0  # Time spent calling the assigned connection.
    t_val = 0.0  # W0.1: чистое время /api/validate
    answer_source_map: list[dict[str, object]] = []
    final_evidence_packet: dict[str, Any] = {}
    evidence_navigation: list[dict[str, Any]] = []
    if topic_retrieval_plan:
        evidence_navigation.append({
            "kind": "topic_selection",
            "available": True,
            "selected_files": len(topic_doc_filter),
            "context_role": "navigation",
            "is_evidence": False,
        })
    if dataset_memory_prompt:
        evidence_navigation.append({
            "kind": "dataset_memory",
            "available": True,
            "context_role": "navigation",
            "is_evidence": False,
        })
    if notebook_study_prompt:
        evidence_navigation.append({
            "kind": "notebook_study",
            "available": True,
            "context_role": "navigation",
            "is_evidence": False,
        })
    if target_file_ref:
        evidence_navigation.append({
            "kind": "target_file",
            "available": target_file_ref.get("match_status") == "matched",
            "match_status": str(target_file_ref.get("match_status") or ""),
            "context_role": "navigation",
            "is_evidence": False,
        })
    deterministic_evidence: list[dict[str, Any]] = []
    if project_inventory_payload:
        deterministic_evidence.append({
            "kind": "project_inventory",
            "source": "metadb.documents",
            "file_count": int(project_inventory_payload.get("file_count") or 0),
        })

    model_evidence_chunks = list(llm_chunks)

    def _build_model_evidence(current_chunks: Sequence[Any]):
        packet = build_retrieval_evidence_packet(
            question=req.question,
            chunks=current_chunks,
            retrieval_trace=retrieval_trace,
            navigation=evidence_navigation,
            deterministic_evidence=deterministic_evidence,
        )
        rendered = render_retrieval_evidence_for_model(
            packet,
            max_chars=context_chars_limit,
            include_metadata=True,
        )
        source_map = packet.source_map(
            max_chars=context_chars_limit,
            include_metadata=True,
        )
        return (
            packet,
            rendered,
            source_map,
            packet.to_dict(max_chars=context_chars_limit, include_metadata=True),
        )

    (
        initial_evidence_packet,
        context,
        answer_source_map,
        final_evidence_packet,
    ) = _build_model_evidence(model_evidence_chunks)
    retrieval_trace["evidence_packet"] = initial_evidence_packet.trace_summary(
        max_chars=context_chars_limit,
        include_metadata=True,
    )
    attachment_context = str(getattr(req, "attachment_context", "") or "").strip()
    if table_tool is not None:
        attachment_context = table_manifest(table_tool)
    terminal_model_answer = None
    terminal_context_packet = None
    active_model_streamed = False
    terminal_answer_streamed = False
    dialogue_context = [session_block] if session_block else []
    from proxy.services.memory_service import session_dialogue_messages
    dialogue_context = session_dialogue_messages(req.session_id) or dialogue_context

    selector_evidence = selector_evidence_payload(
        attachment_context=attachment_context,
        rendered_context=initial_selector_context(
            "" if not document_grounding_enabled and not model_evidence_chunks else context,
            model_authored_initial_query=False,
        ),
    )
    selector_source_map = answer_source_map
    await chat_progress(token_sink, "queue", "Ожидаю свободный слот модели")
    try:
        async with httpx.AsyncClient(timeout=300.0) as client:
            answer = ""
            crag_status = "UNKNOWN"
            tokens = 0
            active_model_result = None
            active_pending_tool_calls = 0
            if connection_resolver is None or connection_secret_store is None:
                raise HTTPException(503, "MODEL_CONNECTION_RESOLVER_REQUIRED")
            bound_runner = BoundModelChatRunner(
                resolver=connection_resolver,
                transport=model_connection_transport(client, connection_secret_store),
                connection_guard=lambda connection: generation_guard(state, connection, token_sink=token_sink),
            )

            async def _post_llm(body, *, allow_stream: bool = True):
                """Один вызов LLM. token_sink задан → стрим (токены клиенту по
                мере генерации), иначе — обычный POST (поведение неизменно).
                Возвращает (answer_text, usage_dict)."""
                nonlocal active_model_result, active_pending_tool_calls, active_model_streamed
                from proxy.services.chat_attachment_service import with_image_attachment
                section_reader.assert_for_inference()
                provider_messages = tuple(body.get("messages") or ())
                provider_messages = with_image_attachment(provider_messages, getattr(req, "attachment_id", None))
                inference_request = InferenceRequest(
                    messages=provider_messages,
                    reasoning_enabled=execution_preset.reasoning_enabled,
                    max_output_tokens=max(
                        1,
                        int(
                            body.get("max_completion_tokens")
                            or body.get("max_tokens")
                            or 1
                        ),
                    ),
                    temperature=body.get("temperature"),
                    tools=tuple(body.get("tools") or ()),
                    response_format=body.get("response_format"),
                )

                active_model_streamed = False
                async def forward_stream(event):
                    nonlocal active_model_streamed
                    if event.get("event") == "token":
                        if execution_preset.reasoning_enabled and not active_model_streamed:
                            await chat_progress(token_sink, "answer", "Модель пишет ответ")
                        active_model_streamed = True
                    elif event.get("event") == "reset":
                        active_model_streamed = False
                    await token_sink(event)

                sensitivities = _dataset_sensitivities(
                    set(_dataset_ids_from_chunks(chunks))
                    | {str(item) for item in (_dataset_ids or [])}
                )
                active_model_result = await run_bound_inference(
                    bound_runner, inference_request,
                    token_sink=forward_stream if token_sink is not None else None,
                    remote_allowed=cloud_allowed(
                        sensitivities,
                        consent=_env_bool("LES_CLOUD_CONSENT", False),
                    ),
                )
                active_pending_tool_calls += active_model_result.pending_tool_calls
                if token_sink is not None and allow_stream and not active_model_streamed and active_model_result.response.text:
                    await token_sink(
                        {"event": "token", "data": active_model_result.response.text}
                    )
                retrieval_trace["model_connection"] = (
                    active_model_result.public_connection_payload()
                )
                retrieval_trace["model_connection"]["pending_tool_calls"] = active_pending_tool_calls
                model_text = active_model_result.response.text
                if active_model_result.response.tool_calls:
                    if token_sink is not None and active_model_streamed:
                        await token_sink({"event": "reset", "data": ""})
                        await token_sink({"event": "progress", "data": {"label": "Проверяю источники выбранными инструментами"}})
                        active_model_streamed = False
                    canonical_calls: list[dict[str, Any]] = []
                    for raw_call in active_model_result.response.tool_calls:
                        function = raw_call.get("function") or {}
                        raw_arguments = function.get("arguments") or "{}"
                        try:
                            arguments = (
                                json.loads(raw_arguments)
                                if isinstance(raw_arguments, str)
                                else dict(raw_arguments)
                            )
                        except (TypeError, ValueError, json.JSONDecodeError):
                            arguments = {}
                        canonical_calls.append(
                            {
                                "call_id": str(raw_call.get("id") or ""),
                                "tool": str(function.get("name") or ""),
                                "args": arguments,
                            }
                        )
                    model_text = json.dumps(
                        {"calls": canonical_calls},
                        ensure_ascii=False,
                        separators=(",", ":"),
                    )
                return (
                    model_text,
                    dict(active_model_result.response.usage),
                )

            tool_results_for_model: list[dict[str, Any]] = []

            tool_context = ""
            visual_tool_requested = any(
                marker in str(req.question or "").casefold().replace("ё", "е")
                for marker in ("посмотри глазами", "посмотри чертеж", "посмотри схему", "что видно на лист", "что изображено на лист")
            )
            profile_tools = [
                str(name) for name in (profile_snapshot or {}).get("tools", []) if str(name).strip()
            ]
            web_research_config = capture_web_research_config()
            selected_sources_only = bool(
                getattr(req, "selected_sources_only", False)
            )
            profile_tools = filter_profile_tools(
                profile_tools,
                selected_sources_only=selected_sources_only,
            )
            profile_tools = web_tools_for_request(
                profile_tools,
                web_research_config,
            )
            retrieval_trace["capability_scope"] = {
                "selected_sources_only": selected_sources_only,
                "public_web_available": not selected_sources_only,
                "web_research": public_web_research_config(web_research_config),
                "web_reader_available": (
                    not selected_sources_only
                    and True
                    and "web_read" in profile_tools
                ),
                "source": "explicit_or_frozen_request",
            }
            profile_tools = tools_for_document_scope(
                profile_tools,
                enabled=document_grounding_enabled,
            )
            if table_tool is not None:
                profile_tools = [TABLE_TOOL, *profile_tools]
            route_trace = canonical_route_trace_payload(
                canonical_route,
                execution_mode=canonical_execution_mode,
                candidate_acceptance=candidate_acceptance,
            )
            retrieval_trace["canonical_route"] = route_trace
            retrieval_trace["route_comparison"] = {
                "schema": "les.canonical-route-comparison.v1",
                "requested": route_trace["requested"],
                "effective": route_trace["effective"],
                "legacy_output_authoritative": (
                    canonical_execution_mode is not CanonicalRouteMode.ACTIVE
                ),
                "same_request": True,
                "profile_revision": str(
                    (profile_snapshot or {}).get("revision_id") or ""
                ),
                "canonical_provider_calls_added": 0,
                "persisted_effects": 0,
            }
            if candidate_acceptance:
                retrieval_trace["route_comparison"]["candidate_acceptance"] = True
            canonical_shadow_recorded = False
            tool_loop_enabled = bool(profile_tools)
            if tool_loop_enabled:
                retrieval_limits = effective_retrieval_policy(profile_snapshot)
                tool_loop_stage = "harness"
                try:
                    from proxy.services.tool_harness_service import harness

                    tool_harness = harness(web_config=web_research_config)
                    table_contract = table_tool.register(tool_harness) if table_tool is not None else None

                    async def _fallback_model_tool(tool_name: str, args: dict[str, Any]):
                        return await asyncio.to_thread(tool_harness.call, tool_name, args)

                    model_research_tools = ModelResearchToolService(
                        retrieve=retrieve_chat_chunks,
                        frozen_dataset_ids=tuple(str(item) for item in _dataset_ids if str(item)),
                        retrieval_kwargs={
                            "rag_backend": rag_backend,
                            "reranker_enabled": _reranker_on,
                            "reranker_available": state.reranker_available,
                            "reranker_cls": state.reranker_cls,
                            "mlx_url": os.getenv("MLX_URL", "http://127.0.0.1:8080"),
                            "logger": logger,
                            "llm_semaphore": state.llm_semaphore,
                            "return_trace": True,
                            "doc_filter": None,
                            "scope_source": str(
                                (scope_resolution or {}).get("scope_source") or "unspecified"
                            ),
                            "scope_error_code": str(
                                (scope_resolution or {}).get("error_code") or ""
                            ),
                        },
                        fallback=_fallback_model_tool,
                        **retrieval_limits,
                    )
                    shortlist_limit = (
                        min(
                            execution_preset.max_tools,
                            max(1, _env_int("LES_CHAT_TOOL_SHORTLIST_LIMIT", 64)),
                        )
                    )
                    configured_call_limit = max(
                        1, _env_int("LES_CHAT_TOOL_MAX_CALLS", 48)
                    )
                    max_calls = (
                        min(execution_preset.max_batch_items, configured_call_limit)
                    )
                    tool_loop_stage = "shortlist"
                    runtime_available_tools = set(profile_tools)
                    shortlist = await asyncio.to_thread(
                        tool_harness.shortlist,
                        req.question,
                        mode=str(req.mode or route.intent or ""),
                        allowed_tools=profile_tools,
                        limit=shortlist_limit,
                        dataset_ids=tuple(str(item) for item in _dataset_ids if str(item)),
                        workflow_phase="research",
                        model_preset=execution_preset.preset_id,
                        runtime_available=frozenset(runtime_available_tools),
                        calls_remaining=max_calls,
                        result_chars_remaining=35_000,
                        **(
                            {"attachment_ids": (str(req.attachment_id),)}
                            if getattr(req, "attachment_id", None)
                            else {}
                        ),
                    )
                    allowed_tools = {
                        str(tool.get("name") or "")
                        for tool in shortlist.get("tools", [])
                        if isinstance(tool, dict) and tool.get("name")
                    }
                    if table_contract is not None:
                        # The explicit attachment grants only this bound document tool.
                        shortlist["tools"] = [table_contract, *[
                            item for item in shortlist.get("tools", []) if item.get("name") != TABLE_TOOL
                        ]][:shortlist_limit]
                        allowed_tools = {item["name"] for item in shortlist["tools"]}
                    native_tools = native_model_tool_schemas(
                        shortlist.get("tools") or []
                    )
                    selected_calls: list[dict[str, Any]] = []
                    selector_usage: list[dict[str, Any]] = []
                    research_rounds: list[dict[str, Any]] = []
                    failed_calls: set[str] = set()
                    completed_reads: set[str] = set()
                    def iteration_call_identity(call):
                        identity = tool_call_identity(call)
                        if table_tool is not None and call.get('tool') == TABLE_TOOL:
                            state = table_tool.state()
                            return f"{state['task_id']}:{state['revision']}:{identity}"
                        return identity
                    research_deadline_seconds = max(
                        1.0,
                        _env_float("LES_CHAT_RESEARCH_DEADLINE_SECONDS", 120.0),
                    )
                    research_deadline = time.monotonic() + research_deadline_seconds
                    research_round = 0
                    calls_remaining = max_calls
                    stop_reason = "deadline"
                    while time.monotonic() < research_deadline and calls_remaining > 0:
                        research_round += 1
                        prior_results = [
                            _compact_tool_result_for_prompt(item, max_chars=2400)
                            for item in without_table_replays(tool_results_for_model[-max_calls:])
                        ]
                        tool_call_instruction = (
                            "Вызывай предоставленные инструменты напрямую. "
                            if canonical_execution_mode is CanonicalRouteMode.ACTIVE
                            else "Верни только JSON {\"calls\":[{\"tool\":\"...\",\"args\":{...}}]}. "
                        )
                        selector_profile = "".join(
                            (
                                profile_tool_selector_prompt(profile_snapshot),
                                "\n\nТы управляешь коротким исследовательским чтением LES. ",
                                "Явно прикреплённый текст ниже уже доступен тебе как evidence и не требует индексации. "
                                "Сначала прочитай его и используй read-only инструменты, чтобы закрыть конкретные пробелы. "
                                "Если оператор просит артефакт, после получения достаточного evidence вызови "
                                "подходящий draft-инструмент; предметные решения и поисковые запросы выбираешь ты. "
                                "Не объявляй вложение отсутствующим, когда его текст присутствует в пакете. ",
                                "Если оператор явно просит посмотреть глазами страницу или лист PDF, ",
                                "обязательно выбери look_at_pdf_page с указанными файлом и номером страницы; ",
                                "текстовый read_pdf_source не заменяет просмотр пикселей. ",
                                "Инструменты не отвечают за тебя и не заменяют источники. ",
                                tool_call_instruction,
                                "Если evidence достаточно, заверши исследование без нового вызова. ",
                                "Не выбирай инструмент вне списка и не выходи за выбранные dataset/file scope.",
                            )
                        )
                        selector_checkpoint = tuple(
                            item
                            for candidate in memory_candidates
                            if candidate.kind == ContextKind.CHECKPOINT
                            for item in candidate.objects
                        )
                        selector_profile = sys_normal
                        selector_working_memory = tuple(
                            item
                            for candidate in memory_candidates
                            if candidate.kind == ContextKind.WORKING_MEMORY
                            for item in candidate.objects
                        )
                        selector_checkpoint = ()
                        selector_working_memory = workspace_memory_objects(
                            memory_candidates, registered=workspace_memory_registered,
                        )
                        tool_loop_stage = "context_governor"
                        selector_messages, selector_packet = govern_inference_messages(
                            preset=execution_preset,
                            profile_prefix=selector_profile,
                            request_payload=req.question,
                            shortlist=selector_context_shortlist(
                                shortlist.get("tools") or [],
                                native_tool_schemas=(
                                    canonical_execution_mode is CanonicalRouteMode.ACTIVE
                                ),
                            ),
                            checkpoint=selector_checkpoint,
                            working_memory=selector_working_memory,
                            evidence=selector_evidence,
                            source_map=selector_source_map,
                            tool_exchange=prior_results,
                            dialogue=dialogue_context,
                            **table_tool.model_context() if table_tool is not None else {},
                        )
                        retrieval_trace["context_governor"]["calls"].append(
                            {**context_packet_trace(selector_packet, purpose="tool_decision"), "sent_to_model": True}
                        )
                        selector_body = {
                            "messages": selector_messages,
                            "stream": False,
                            "temperature": 0,
                            "max_tokens": (execution_preset.generation_reserve_tokens),
                        }
                        selector_body["tools"] = native_tools
                        t_tool_selector = time.time()
                        tool_loop_stage = "selector_model"
                        selector_text, round_usage = await _post_llm(
                            selector_body,
                            allow_stream=False,
                        )
                        t_llm += time.time() - t_tool_selector
                        selector_usage.append(round_usage)
                        tool_loop_stage = "selector_parse"
                        proposed_calls = [
                            _augment_model_tool_args(
                                call,
                                question=req.question,
                                dataset_ids=[str(d) for d in _dataset_ids],
                                target_file_ref=target_file_ref,
                            )
                            for call in _parse_model_tool_calls(
                                selector_text,
                                allowed_tools=allowed_tools,
                                max_calls=calls_remaining,
                            )
                        ]
                        calls = [call for call in proposed_calls[:calls_remaining]
                                 if iteration_call_identity(call) not in failed_calls
                                 and iteration_call_identity(call) not in completed_reads]
                        research_rounds.append(
                            {"round": research_round, "proposed": len(proposed_calls), "executed": len(calls)}
                        )
                        if not calls:
                            stop_reason = "model_stop"
                            if proposed_calls:
                                stop_reason = "repeated_tool_failure"
                                if any(iteration_call_identity(call) in completed_reads for call in proposed_calls):
                                    stop_reason = "repeated_read"
                            else:
                                response = active_model_result.response
                                if response.text.strip() and not response.tool_calls:
                                    terminal_model_answer = (response.text, dict(response.usage))
                                    terminal_answer_streamed = active_model_streamed
                                    terminal_context_packet = selector_packet
                            break
                        for call in calls:
                            call_identity = iteration_call_identity(call)
                            tool_loop_stage = "tool_execution"
                            tool_name = str(call.get("tool") or "")
                            await chat_progress(token_sink, "tool", "Читаю и проверяю найденные материалы")
                            research_result = await model_research_tools.execute(call)
                            payload = research_result.payload
                            if table_tool is not None and tool_name == TABLE_TOOL and payload.get('status') == 'error':
                                table_tool.remember_validation_failure(call.get('args') or {}, payload)
                            if research_result.chunks:
                                known_chunk_ids = {
                                    str((getattr(item, "meta", {}) or {}).get("chunk_id") or "")
                                    or hashlib.sha256(
                                        (
                                            str(getattr(item, "doc_name", "") or "")
                                            + "\x00"
                                            + str(getattr(item, "content", "") or "")
                                        ).encode("utf-8")
                                    ).hexdigest()
                                    for item in chunks
                                }
                                for found_chunk in research_result.chunks:
                                    found_id = str(
                                        (getattr(found_chunk, "meta", {}) or {}).get("chunk_id") or ""
                                    ) or hashlib.sha256(
                                        (
                                            str(getattr(found_chunk, "doc_name", "") or "")
                                            + "\x00"
                                            + str(getattr(found_chunk, "content", "") or "")
                                        ).encode("utf-8")
                                    ).hexdigest()
                                    if found_id not in known_chunk_ids:
                                        chunks.append(found_chunk)
                                        known_chunk_ids.add(found_id)
                                research_windows = await expand_chat_context(
                                    chunks,
                                    collection=getattr(rag_backend, "collection_name", ""),
                                    logger=logger,
                                    max_chunks=context_max_chunks,
                                    max_chars_per_chunk=context_window_chars,
                                    radius=context_radius,
                                )
                                model_evidence_chunks = list(research_windows.chunks)
                                (
                                    _research_evidence_packet,
                                    context,
                                    answer_source_map,
                                    final_evidence_packet,
                                ) = _build_model_evidence(model_evidence_chunks)
                                retrieval_trace["evidence_packet"] = (
                                    _research_evidence_packet.trace_summary(
                                        max_chars=context_chars_limit,
                                        include_metadata=True,
                                    )
                                )
                                selector_evidence = selector_evidence_payload(
                                    attachment_context=attachment_context,
                                    rendered_context=context,
                                )
                                selector_source_map = answer_source_map
                            selected_calls.append(call)
                            if payload.get("status") == "error" or payload.get("ok") is False:
                                failed_calls.add(call_identity)
                            completed_reads.add(call_identity)
                            calls_remaining -= 1
                            tool_results_for_model.append(payload)
                            if table_tool is not None and tool_name == TABLE_TOOL:
                                task_status = table_tool.state()
                                retrieval_trace["document_task"] = task_status
                                done = task_status['total'] - task_status['pending']
                                await chat_progress(token_sink, "document", f"Проверено строк: {done} из {task_status['total']}")
                            answer_source_map = _merge_web_source_map(
                                answer_source_map,
                                tool_results_for_model,
                            )
                            selector_source_map = answer_source_map

                        if calls_remaining <= 0:
                            stop_reason = "calls_budget"
                            break
                    tool_context = _format_tool_results_for_model(without_table_replays(tool_results_for_model))
                    web_research_trace = public_web_research_config(
                        web_research_config
                    )
                    for web_payload in tool_results_for_model:
                        if str(web_payload.get("tool") or "") != "web_search":
                            continue
                        web_result = web_payload.get("result") or {}
                        if isinstance(web_result, Mapping):
                            web_research_trace.update(
                                {
                                    "requested_mode": str(
                                        web_result.get("requested_mode")
                                        or web_research_config.mode
                                    ),
                                    "effective_mode": str(
                                        web_result.get("effective_mode")
                                        or web_research_config.mode
                                    ),
                                    "provider": str(web_result.get("provider") or ""),
                                    "degraded": bool(web_result.get("degraded")),
                                    "fallback_reason": str(
                                        web_result.get("fallback_reason") or ""
                                    ),
                                }
                            )
                    retrieval_trace["tool_loop"] = {
                        "schema": "les_model_research_loop_v1",
                        "enabled": True,
                        "model_owns_selection": True,
                        "selector_model": llm_model,
                        "selector_provider": "model_connection",
                        "shortlist": shortlist,
                        "selected_calls": [
                            safe_selected_call_trace(call) for call in selected_calls
                        ],
                        "selector_usage": selector_usage,
                        "results": tool_results_for_model,
                        "rounds": research_rounds,
                        "stop_reason": stop_reason,
                        "deadline_seconds": research_deadline_seconds,
                        "max_calls_per_model_response": max_calls,
                        "max_calls_total": max_calls,
                        "calls_remaining": calls_remaining,
                        "native_tool_schemas": bool(native_tools),
                        "web_research": web_research_trace,
                    }
                except ContextRequiredSectionOverflow as context_error:
                    retrieval_trace["context_governor"]["error"] = {
                        "code": context_error.code,
                        "purpose": "tool_decision",
                        "budget": context_error.budget,
                        "required_tokens": context_error.required_tokens,
                        "required_objects": len(context_error.object_ids),
                    }
                    raise HTTPException(
                        422,
                        detail={
                            "code": context_error.code,
                            "message": "Обязательная часть выбора инструмента не помещается в безопасный контекст модели.",
                        },
                    ) from context_error
                except SectionReadError:
                    raise
                except Exception as tool_err:  # noqa: BLE001 - optional tool errors remain observable in trace
                    logger.exception(
                        "[TOOLS] model tool loop skipped: %s",
                        type(tool_err).__name__,
                    )
                    retrieval_trace["tool_loop"] = {
                        "schema": "les_model_tool_loop_v1",
                        "enabled": True,
                        "status": "error",
                        "error_type": type(tool_err).__name__,
                        "error_stage": tool_loop_stage,
                        "recorded_results": len(tool_results_for_model),
                    }
                    if isinstance(tool_err, ModelTransportError) and str(tool_err) == "CAPABILITY_REQUIRED: tools":
                        retrieval_trace["tool_loop"]["unavailable_capability"] = "tools"
                    elif isinstance(tool_err, ModelTransportError):
                        # Preserve outage/timeout instead of repeating the same failed inference.
                        raise
            else:
                retrieval_trace["tool_loop"] = {
                    "schema": "les_model_research_loop_v1",
                    "enabled": False,
                    "reason": "disabled_by_operator",
                    "model_owns_final_answer": True,
                }
            # Keep the connection/preset-owned split between input and output.
            # Reserving 4096 tokens unconditionally for model-authored rows made
            # the profile + request overflow an 8K Qwen window before evidence
            # could reach the model at all.
            answer_execution_preset = execution_preset
            max_attempts = 2
            for attempt in range(1, max_attempts + 1):
                if attempt == 2:
                    # Ретрай не выбрасывает найденные источники: повторная генерация получает
                    # весь уже собранный evidence packet, а не новый кодовый shortlist.
                    strict_chunks = list(chunks)
                    strict_windows = await expand_chat_context(
                        strict_chunks if strict_chunks else chunks[:2],
                        collection=getattr(rag_backend, "collection_name", ""),
                        logger=logger,
                        max_chunks=None,
                    )
                    ctx_chunks = strict_windows.chunks
                    evidence_packet = build_retrieval_evidence_packet(
                        question=req.question,
                        chunks=ctx_chunks,
                        retrieval_trace=retrieval_trace,
                        navigation=evidence_navigation,
                        deterministic_evidence=deterministic_evidence,
                    )
                    context = render_retrieval_evidence_for_model(
                        evidence_packet,
                        max_chars=context_chars_limit,
                        include_metadata=True,
                    )
                    answer_source_map = evidence_packet.source_map(max_chars=context_chars_limit, include_metadata=True)
                    final_evidence_packet = evidence_packet.to_dict(max_chars=context_chars_limit, include_metadata=True)
                    retrieval_trace["evidence_packet"] = evidence_packet.trace_summary(
                        max_chars=context_chars_limit,
                        include_metadata=True,
                    )
                    answer_source_map = _merge_web_source_map(
                        answer_source_map,
                        tool_results_for_model,
                    )
                    sys_msg = sys_strict
                    logger.warning("[SAFERAG] Retry #2 — строгий промпт, %s чанков", len(ctx_chunks))
                else:
                    ctx_chunks = model_evidence_chunks
                    (
                        evidence_packet,
                        context,
                        answer_source_map,
                        final_evidence_packet,
                    ) = _build_model_evidence(ctx_chunks)
                    retrieval_trace["evidence_packet"] = evidence_packet.trace_summary(
                        max_chars=context_chars_limit,
                        include_metadata=True,
                    )
                    answer_source_map = _merge_web_source_map(
                        answer_source_map,
                        tool_results_for_model,
                    )
                    # ADR-12 §2: каркас формы под интент добавляем к нормальному промпту.
                    sys_msg = sys_normal + (f" {answer_form.instruction}" if answer_form.instruction else "")
                    # Формат/стиль из GUI (глубина/язык) — ТОЛЬКО в системный промпт генерации,
                    # чтобы роутинг/авто-заметки/ретрив видели чистый вопрос (не мусор-директиву).
                    if req.output_directive and req.output_directive.strip():
                        sys_msg += " " + req.output_directive.strip()
                    if target_doc_filter:
                        sys_msg += (
                            " Оператор явно выбрал документы. Отвечай только по их содержимому, "
                            "явно называй использованные файлы и не расширяй область на остальной датасет."
                        )
                question_tail = (
                    f"Вопрос: {req.question}\n\n"
                    "/no_think\n"
                    "Дай итоговый инженерный ответ. Не выдумывай факты и используй только существующие "
                    "номера [Источник N]. Если материалов недостаточно, отдели это от подтверждённых выводов."
                )
                question_tail = req.question
                answer_checkpoint = tuple(
                    item
                    for candidate in memory_candidates
                    if candidate.kind == ContextKind.CHECKPOINT
                    for item in candidate.objects
                )
                answer_working_memory = tuple(
                    item
                    for candidate in memory_candidates
                    if candidate.kind == ContextKind.WORKING_MEMORY
                    for item in candidate.objects
                ) + _text_context_objects("working:legacy", memory_block) + _text_context_objects(
                    "working:project-advisory", project_memory_advisory
                )
                answer_working_memory += _context_objects(
                    "navigation:status", evidence_navigation
                )
                for navigation_name, navigation_text in (
                    ("dataset", dataset_memory_prompt),
                    ("inventory", project_inventory_prompt),
                    ("notebook", notebook_study_prompt),
                    (
                        "selected-documents",
                        "Выбранные документы: " + "; ".join(target_doc_filter) + "."
                        if target_doc_filter else "",
                    ),
                ):
                    answer_working_memory += _text_context_objects(
                        f"navigation:{navigation_name}", navigation_text
                    )
                answer_checkpoint = ()
                answer_working_memory = workspace_memory_objects(
                    memory_candidates, registered=workspace_memory_registered,
                ) + _text_context_objects("attachment:current", attachment_context)
                answer_tool_exchange = (
                    [
                        _compact_tool_result_for_prompt(item, max_chars=2400)
                        for item in without_table_replays(tool_results_for_model)
                    ]
                )
                answer_evidence = (
                    [
                        "Материалы из найденных документов:",
                        *source_context_blocks(context),
                    ] if context.strip() else []
                )
                try:
                    messages, answer_packet = govern_inference_messages(
                        preset=answer_execution_preset,
                        profile_prefix=sys_msg,
                        request_payload=question_tail,
                        checkpoint=answer_checkpoint,
                        working_memory=answer_working_memory,
                        evidence=answer_evidence,
                        source_map=(answer_source_map),
                        tool_exchange=answer_tool_exchange,
                        dialogue=dialogue_context,
                        required_evidence=[table_tool.context()] if table_tool is not None else (),
                    )
                except ContextRequiredSectionOverflow as context_error:
                    retrieval_trace["context_governor"]["error"] = {
                        "code": context_error.code,
                        "budget": context_error.budget,
                        "required_tokens": context_error.required_tokens,
                        "required_objects": len(context_error.object_ids),
                    }
                    raise HTTPException(
                        422,
                        detail={
                            "code": context_error.code,
                            "message": "Обязательная часть запроса не помещается в безопасный контекст модели.",
                        },
                    ) from context_error
                retrieval_trace["context_governor"]["calls"].append(
                    {**context_packet_trace(answer_packet, purpose="answer"), "sent_to_model": terminal_model_answer is None}
                )
                user_prompt = next(
                    (message["content"] for message in messages if message["role"] == "user"),
                    "",
                )

                prompt_layers = {
                    "system": len(sys_msg),
                    "evidence": len(context),
                    "tools": len(tool_context),
                    "dataset_navigation": len(dataset_memory_prompt),
                    "inventory_navigation": len(project_inventory_prompt),
                    "notebook_navigation": len(notebook_study_prompt),
                    "session_memory": len(session_block),
                    "working_memory": len(memory_block),
                    "project_memory_advisory": len(project_memory_advisory),
                    "question": len(req.question),
                    "user_total": len(user_prompt),
                    "messages_total": sum(len(str(message.get("content") or "")) for message in messages),
                }
                retrieval_trace["prompt_layers"] = prompt_layers
                logger.info(
                    "[PROMPT] provider=%s model=%s attempt=%s chars=%s layers=%s",
                    "model_connection",
                    llm_model,
                    attempt,
                    prompt_layers["messages_total"],
                    prompt_layers,
                )

                generation_budget = _generation_token_budget(
                    max_tokens=answer_form.max_tokens,
                    local_big=local_big,
                    attempt=attempt,
                    intent=answer_form.intent,
                )
                if notebook_study_prompt:
                    generation_budget = min(
                        generation_budget,
                        _env_int("LES_NOTEBOOK_STUDY_MAX_TOKENS", 2048),
                    )
                if project_inventory_prompt:
                    generation_budget = min(
                        generation_budget,
                        _env_int("LES_PROJECT_INVENTORY_MAX_TOKENS", 3072),
                    )
                generation_budget = min(
                    generation_budget,
                    answer_execution_preset.generation_reserve_tokens,
                )

                if answer_execution_preset.reasoning_enabled:
                    generation_budget = answer_execution_preset.generation_reserve_tokens

                chat_body = {
                    "messages": messages,
                    "stream": False,
                    "temperature": profile_temperature(
                        profile_snapshot,
                        fallback=_env_float("CHAT_TEMPERATURE", 0.2),
                    ),
                    "max_tokens": generation_budget,
                }
                # При стриминге ретрай (строгий промпт) шлёт уже новый текст —
                # просим клиент очистить накопленное от прошлой попытки.
                if token_sink is not None and attempt > 1:
                    await token_sink({"event": "reset", "data": ""})
                t_llm_call = time.time()
                if terminal_model_answer is not None:
                    answer, usage = terminal_model_answer
                    if token_sink is not None and not terminal_answer_streamed:
                        await token_sink({"event": "token", "data": answer})
                    retrieval_trace["model_terminal_answer_preserved"] = True
                else:
                    answer, usage = await _post_llm(chat_body, allow_stream=True)
                    if active_model_result is not None:
                        llm_model = active_model_result.connection.model_id
                t_llm += time.time() - t_llm_call
                if not answer:
                    if attempt < max_attempts:
                        logger.warning("[CHAT] empty LLM answer on attempt=%s — retrying strict", attempt)
                        continue
                    raise ValueError(f"Пустой ответ LLM (stream={token_sink is not None})")
                actual_packet = terminal_context_packet if terminal_model_answer is not None else answer_packet
                if actual_packet is not None:
                    answer_source_map = model_visible_source_map(actual_packet, answer_source_map)
                    retrieval_trace["section_coverage"] = section_reader.coverage(answer_source_map)
                    visible_labels = {item.get("label") for item in answer_source_map}
                    if "evidence" in final_evidence_packet:
                        final_evidence_packet["evidence"]["sources"] = [
                            item for item in final_evidence_packet["evidence"].get("sources", [])
                            if item.get("context_label") in visible_labels
                        ]
                        final_evidence_packet["retrieval"]["visible_source_count"] = len(final_evidence_packet["evidence"]["sources"])
                if token_sink is not None:
                    shown = visible_chunks(ctx_chunks, answer_source_map)
                    await token_sink({"event": "sources", "data": {
                        "sources": source_names(shown),
                        "source_excerpts": source_excerpts(shown, max_n=len(shown), max_chars=280),
                        "source_map": answer_source_map,
                    }})
                tokens = usage.get("completion_tokens", 0)
                # W3.3: учёт расходов облака (токены → $). Локальные вызовы не считаем.
                if active_model_result is not None and active_model_result.connection.locality is ConnectionLocality.REMOTE:
                    _record_cloud_cost(state, llm_model, usage)
                logger.info(
                    "[CHAT] attempt=%s provider=%s model=%s tokens=%s",
                    attempt,
                    "model_connection",
                    llm_model,
                    tokens,
                )

                crag_status = "MODEL_OUTPUT"
                logger.info("[CHAT] model answer accepted unchanged; citation check is trace-only")
                break


            if table_tool is not None and (getattr(req, 'attachment_id', None) or any(
                    item.get('tool') == TABLE_TOOL for item in tool_results_for_model)):
                notice = table_tool.coverage_notice()
                if notice:
                    answer = notice + '\n\n' + answer
                    await chat_progress(token_sink, 'document', notice.replace('**', ''))

            try:
                from proxy.services.evidence_packet_service import verify_answer_source_labels

                citation_check = verify_answer_source_labels(answer, answer_source_map)
                retrieval_trace["citation_check"] = citation_check
            except Exception as citation_error:  # noqa: BLE001
                retrieval_trace["citation_check"] = {
                    "schema": "les.answer-citation-check.v1",
                    "status": "error",
                    "error": type(citation_error).__name__,
                }

            t_gen = time.time() - t_gen_start

            if crag_status == "HALLUCINATION":
                state.crag_stats["hallucination"] += 1
                state.chat_metrics["crag_fail"] += 1
            elif crag_status == "VERIFIED":
                state.crag_stats["verified"] += 1
                state.chat_metrics["crag_pass"] += 1
            elif crag_status in {"UNVALIDATED", "MODEL_OUTPUT"}:
                state.crag_stats["unvalidated"] = state.crag_stats.get("unvalidated", 0) + 1
                state.chat_metrics["crag_fail"] += 1
            else:
                state.crag_stats["no_data"] += 1
                state.chat_metrics["crag_fail"] += 1

            state.chat_metrics["latency_search"].append(t_search)
            state.chat_metrics["latency_gen"].append(t_gen)
            state.chat_metrics["tokens"].append(tokens)
            # W0.1: пофазная латентность; overhead = очередь семафора + сборка промпта внутри t_gen
            wall_total = time.time() - t_request_start
            phases = {
                "pre_retrieval": round(max(0.0, t_search_start - t_request_start), 3),
                "retrieval": round(t_search, 3),
                "notebook_study": round(notebook_study_latency, 3),
                "context": round(t_ctx, 3),
                "generation": round(t_llm, 3),
                "validation": round(t_val, 3),
                "overhead": round(max(0.0, t_gen - t_llm - t_val), 3),
                "total": round(t_search + notebook_study_latency + t_ctx + t_gen, 3),
                "wall_total": round(wall_total, 3),
            }
            retrieval_trace["latency_phases"] = phases
            retrieval_trace["source_map_count"] = len(answer_source_map)
            tool_candidate_counts = []
            for tool_result in tool_results_for_model:
                tool_trace = (
                    tool_result.get("trace")
                    if isinstance(tool_result, dict)
                    and isinstance(tool_result.get("trace"), dict)
                    else {}
                )
                selection = (
                    tool_trace.get("candidate_selection")
                    if isinstance(tool_trace.get("candidate_selection"), dict)
                    else {}
                )
                if selection.get("found_count") is not None:
                    tool_candidate_counts.append(int(selection["found_count"]))
            retrieval_selection = (
                retrieval_trace.get("candidate_selection")
                if isinstance(retrieval_trace.get("candidate_selection"), dict)
                else {}
            )
            found_count = (
                sum(tool_candidate_counts)
                if tool_candidate_counts
                else int(
                    retrieval_selection.get("found_count")
                    or len(model_evidence_chunks)
                )
            )
            source_counts = evidence_counts(
                answer=answer,
                source_map=answer_source_map,
                found_count=found_count,
            )
            retrieval_trace["source_counts"] = source_counts
            retrieval_trace["source_map"] = answer_source_map
            evidence_manifest = build_evidence_manifest(
                query=str(req.question or ""),
                scope={
                    "dataset_ids": [str(item) for item in _dataset_ids],
                    "dataset_filter": str(effective_dataset_filter or ""),
                    "model_queries": list(
                        (retrieval_trace.get("tool_loop") or {}).get("model_queries")
                        or []
                    ),
                    "selected_sources_only": bool(
                        getattr(req, "selected_sources_only", False)
                    ),
                },
                chunks=model_evidence_chunks,
                answer=answer,
            )
            retrieval_trace["evidence_manifest"] = evidence_manifest
            state.chat_metrics.setdefault("latency_phases", []).append(phases)
            logger.info("[METRICS] phases=%s", phases)
            for key in ("latency_search", "latency_gen", "tokens", "latency_phases"):
                state.chat_metrics[key] = state.chat_metrics[key][-100:]

            history_chunks = visible_chunks(ctx_chunks, answer_source_map)
            sources_list = source_names(history_chunks)
            if project_inventory_prompt:
                sources_list = [*sources_list, "Опись файлов датасета (MetaDB documents)"]
            source_dataset_ids = _dataset_ids_from_chunks(history_chunks)
            source_dataset_names = _names_for_dataset_ids(source_dataset_ids, dataset_name_by_id)
            source_scope = {
                "requested": [
                    str(item) for item in (getattr(req, "dataset_ids", None) or _dataset_ids)
                ],
                "resolved": [str(item) for item in (_dataset_ids or [])],
                "used": source_dataset_ids,
                "used_names": source_dataset_names,
            }
            retrieval_trace["source_scope"] = source_scope
            history_id = None

            try:
                history_id = save_chat_history(
                    question=req.question,
                    attachment_context=attachment_context,
                    answer=answer,
                    sources=sources_list,
                    crag_status=crag_status,
                    latency_sec=wall_total,
                    tokens=tokens,
                    session_id=req.session_id,
                    requested_dataset_filter=req.dataset_filter,
                    effective_dataset_filter=effective_dataset_filter,
                    resolved_dataset_ids=_dataset_ids,
                    resolved_dataset_names=resolved_dataset_names,
                    source_dataset_ids=source_dataset_ids,
                    source_dataset_names=source_dataset_names,
                    query_route=query_route_payload,
                    retrieval_trace=retrieval_trace,
                    artifact=None,
                    cache_type=cache_marker,
                    validation_enabled=use_validation,
                )
            except Exception as db_err:
                logger.warning("[CHAT] History save error: %s", db_err)

            if use_semantic_cache and cache_embedding and cache_scope and crag_status == "VERIFIED":
                try:
                    cache.store(
                        req.question,
                        cache_scope,
                        cache_embedding,
                        answer,
                        sources_list,
                        crag_status,
                    )
                except Exception as cache_err:
                    logger.warning("[SEM_CACHE] store skipped: %s", cache_err)
            elif use_semantic_cache and cache_scope and crag_status == "UNVALIDATED":
                try:
                    cache.store_session_unvalidated(
                        req.question,
                        cache_scope,
                        focused_fingerprint,
                        answer,
                        sources_list,
                        crag_status,
                        req.session_id,
                    )
                except Exception as cache_err:
                    logger.warning("[SESSION_CACHE] store skipped: %s", cache_err)

            # Numeric provenance гард (Codex §8, пет, flag-only): числа в ответе, которых нет
            # в контексте — возможно не заземлённые. Метим, не блокируем. Сбой → пропуск.
            try:
                from proxy.services.saferag_service import numeric_provenance_check
                _num_unverified = numeric_provenance_check(answer, context)
            except Exception:  # noqa: BLE001
                _num_unverified = []

            response: dict[str, Any] = {
                "answer": answer,
                "crag_status": crag_status,
                "sources": sources_list,
                "effective_dataset_filter": effective_dataset_filter,
                "query_route": query_route_payload,
                "retrieval_trace": retrieval_trace,
                "cache": cache_marker,
                "validation": {"enabled": use_validation},
                "history_id": history_id,
                "source_excerpts": source_excerpts(history_chunks),
                "source_map": answer_source_map,
                "evidence_packet": final_evidence_packet,
                "latency_phases": phases,
                "class_suggestions": class_suggestions,
                "versions": _version_stamp(),
                "numeric_unverified": _num_unverified,
                "source_scope": source_scope,
                "source_counts": source_counts,
            }
            if active_model_result is not None:
                response["model_connection"] = active_model_result.public_connection_payload()
                response["model_connection"]["pending_tool_calls"] = active_pending_tool_calls
                actual_connection = active_model_result.connection
                response["versions"].update({
                    "llm_provider": getattr(actual_connection, "extension_type", None) or "openai-compatible",
                    "llm_model": actual_connection.model_id,
                    "connection_revision": actual_connection.revision_id,
                })
            if notebook_study_pack is not None:
                response["notebook_context"] = notebook_study_pack.payload()
                if notebook_study_artifact:
                    response["artifact"] = {
                        "title": "Инженерный блокнот",
                        "mode": "markdown",
                        "content": notebook_study_artifact,
                    }
            if dataset_memory_prompt:
                response["dataset_memory"] = {
                    "schema": "dataset_memory_context_v1",
                    "context_role": "navigation",
                    "is_evidence": False,
                }
            if project_inventory_prompt:
                response["project_inventory"] = project_inventory_payload or {}
                if notebook_study_pack is not None and notebook_study_artifact:
                    response["notebook_artifact"] = {
                        "title": "Инженерный блокнот",
                        "mode": "markdown",
                        "content": notebook_study_artifact,
                    }
            if project_inventory_prompt:
                response["artifact"] = {
                    "title": "Реестр файлов",
                    "mode": "markdown",
                    "content": "```text\n" + (project_inventory_artifact_text or project_inventory_prompt).replace("```", "'''") + "\n```",
                    "project_inventory": project_inventory_payload or {},
                }

            # W6.7: source_id CAD/BIM-элементов из текста чанков → ответ + снимок
            # подсветки. Вьювер АТЛАС поллит /api/cad-bim/highlight и перекрашивает.
            # The only ordinary-RAG write hook. It runs after a successful
            # response is complete and performs at most a durable queue INSERT.
            try:
                evidence_sources = list(
                    ((final_evidence_packet.get("evidence") or {}).get("sources") or [])
                )
                memory_refs = [
                    {
                        "ref_id": str(item.get("id") or ""),
                        "doc_id": str(item.get("doc_id") or item.get("doc_name") or ""),
                        "locator": json.dumps(
                            item.get("locator") or {}, ensure_ascii=False, sort_keys=True
                        ),
                        "source_revision": str(item.get("source_version") or ""),
                        "is_evidence": bool(item.get("is_evidence")),
                        "snippet_sha256": "",
                    }
                    for item in evidence_sources
                    if isinstance(item, dict) and item.get("is_evidence")
                ]
                if not workspace_memory_registered:
                    get_memory_port().enqueue_rag_turn(
                        memory_project_id,
                        {
                            "question": str(req.question or ""),
                            "answer": answer,
                            "crag_status": crag_status,
                            "query_route": query_route_payload,
                            "evidence_refs": memory_refs,
                            "retrieval_fingerprint": focused_fingerprint,
                            "cache_hit": False,
                        },
                    )
            except Exception as memory_error:  # queue pressure cannot fail chat
                logger.warning("[MEMORY] grounded turn enqueue skipped: %s", memory_error)

            cad_bim_ids, cad_bim_import_id = extract_highlight(
                getattr(chunk, "content", "") or "" for chunk in chunks
            )
            if cad_bim_ids:
                response["source_ids"] = cad_bim_ids
                response["cad_bim"] = {
                    "import_id": cad_bim_import_id,
                    "source_ids": cad_bim_ids,
                }
                try:
                    set_highlight(cad_bim_ids, import_id=cad_bim_import_id, question=req.question)
                except Exception as hl_err:  # подсветка не должна ронять ответ
                    logger.warning("[CHAT] highlight store skipped: %s", hl_err)

            return response

    except (HTTPException, GenerationSlotTimeout):
        raise
    except httpx.TimeoutException as e:
        logger.error("[CHAT] LLM TIMEOUT: %s", e)
        raise HTTPException(504, "Истёк таймаут назначенной модели — проверь подключение или повтори запрос.")
    except ModelTransportError as e:
        logger.error("[CHAT] ASSIGNED MODEL ERROR: %s", e)
        raise HTTPException(502, str(e)) from e
    except httpx.HTTPStatusError as e:
        detail = f"LLM HTTP {e.response.status_code}: {e.response.text[:200]}"
        logger.error("[CHAT] LLM HTTP ERROR: %s", detail)
        raise HTTPException(502, detail)
    except httpx.ConnectError as e:
        logger.error("[CHAT] LLM CONNECT ERROR: %s", e)
        raise HTTPException(503, f"LLM недоступен ({resolved_connection.base_url}) — проверь назначенную модель и её подключение.")
    except Exception as e:
        import traceback

        logger.error("[CHAT] UNEXPECTED ERROR: %s\n%s", e, traceback.format_exc())
        raise HTTPException(500, f"{type(e).__name__}: {e}")
