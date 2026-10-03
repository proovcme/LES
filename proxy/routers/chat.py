"""SafeRAG chat route."""

from __future__ import annotations

import asyncio
import hashlib
import logging
import os
import re
import sqlite3
import time
import json
from contextvars import ContextVar
from dataclasses import dataclass
from pathlib import Path
from typing import Annotated, Any, Callable, Iterable, List, Optional
from uuid import uuid4

import httpx
from fastapi import APIRouter, Depends, Header, HTTPException, Query
from fastapi.responses import FileResponse, StreamingResponse
from pydantic import BaseModel, field_validator

from backend.rag_config import rag_meta_db_path
from backend.runtime_paths import mutable_path
from proxy.config import ENV_PATH
from proxy.security import require_user
from proxy.services.answer_form_service import classify_answer_form
from proxy.services.chat_evidence_application_service import (
    EvidenceRequestContext,
    EvidenceRuntimeDeps,
    ResponseBoundary,
    run_chat_evidence_application,
)
from proxy.services.answer_contract_service import decorate_payload, scenario_for_request
from proxy.services.class_router_service import build_class_suggestions
from proxy.services.chat_provider_session_service import ChatProviderConfig
from proxy.services.canonical_route_service import (
    BoundModelChatRunner,
    CanonicalRouteMode,
)
from proxy.services.canonical_promotion_service import resolve_promoted_route
from proxy.services.candidate_acceptance_service import (
    CandidateAcceptanceError,
    require_candidate_acceptance,
)
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_capability_service import CapabilityProbe
from proxy.services.model_connection_contracts import CapabilityName, ConnectionRole
from proxy.services.model_connection_resolver_service import (
    ModelConnectionResolutionError,
    ModelConnectionResolver,
)
from proxy.services.model_secret_service import EnvironmentSecretStore
from proxy.services.openai_compatible_transport_service import (
    InferenceRequest,
    InferenceResponse,
    OpenAICompatibleTransport,
)
from backend.inference.validator import rules_pre_verdict
from backend.inference.routing import (
    decide_provider,
    estimate_cost_usd,
    is_cloud_provider,
    load_price_table_from_env,
)
from proxy.services.cad_bim_highlight import extract_highlight, set_highlight
from proxy.services.clause_lookup_service import maybe_answer_clause_lookup
from proxy.services.context_expander_service import expand_context_windows
from proxy.services.context_memory_service import build_context_memory_block, update_chat_profile
from proxy.services.evidence_packet_service import (
    build_retrieval_evidence_packet,
    render_retrieval_evidence_for_model,
)
from proxy.services.memory_service import (
    session_memory, session_recent_retrieval_traces, session_user_questions)
from proxy.services.kot_service import analyze_question
from proxy.services.lexical_index_service import retrieval_fingerprint
from proxy.local_model_registry import DEFAULT_LOCAL_MLX_MODEL
from proxy.services.notebook_study_service import (
    build_notebook_study_pack,
    format_study_artifact,
    is_notebook_study_query,
    prompt_block as notebook_study_prompt_block,
)
from proxy.services.dataset_memory_service import (
    get_typed_dataset_memory,
    run_dataset_reader_pass,
    schedule_dataset_reader_pass,
    select_topic_retrieval_plan,
)
from proxy.services.notebook_service import dataset_memory_prompt_excerpt
from proxy.services.project_summary_service import (
    build_project_summary,
    format_project_inventory_context,
    format_project_inventory_prompt,
    is_project_inventory_query,
    resolve_inventory_file_reference,
)
from proxy.services.prompt_registry_service import build_mode_system_prompt
from proxy.services.llm_transport_profile_service import (
    apply_transport_options,
    assistant_delta_text,
    provider_prompt_max_chars,
    provider_is_local,
)
from proxy.services.query_router import route_query
from proxy.services.retrieval_service import resolve_dataset_ids, retrieve_chat_chunks
from proxy.services.runtime_admission import (
    GenerationSlotTimeout,
    count_active_jobs,
    evaluate_chat_admission,
)
from proxy.services.public_error_service import public_error_payload
from proxy.services.runtime_dispatcher import RuntimeDispatcher
from proxy.services.saferag_service import (
    SAFE_FALLBACK,
    build_context,
    build_validation_context,
    concentrate_sources,
    rank_chunks_for_question,
    source_map_for_context,
    source_names,
)
from proxy.services.semantic_cache import (
    SemanticCache,
    dataset_scope_key,
    embed_question,
    semantic_cache_enabled,
    semantic_cache_threshold,
)
from proxy.services.table_query_service import maybe_answer_table_query, parquet_ref_chunks_for_datasets

from proxy.services import chat_inference_service
from proxy.services import chat_persistence_service
from proxy.services import chat_prompt_support
from proxy.services import chat_request_contracts
from proxy.services import chat_request_service
from proxy.services import chat_runtime
from proxy.services.chat_runtime import set_chat_state
from proxy.services.chat_runtime import ChatRouterState
from proxy.services.chat_runtime import get_chat_state
from proxy.services.chat_request_contracts import ChatRequest
from proxy.services.chat_request_contracts import _require_candidate_acceptance
from proxy.services.chat_inference_service import _is_local_llm_url
from proxy.services.chat_inference_service import _model_capability_probe
from proxy.services.chat_inference_service import _env_int
from proxy.services.chat_inference_service import DEFAULT_OPENAI_MODEL
from proxy.services.chat_inference_service import _REQUEST_CLOUD_CONSENT
from proxy.services.chat_inference_service import _refresh_stale_bound_model_capabilities
from proxy.services.chat_inference_service import _join_openai_path
from proxy.services.chat_inference_service import _record_cloud_cost
from proxy.services.chat_inference_service import _REQUEST_LLM_RUNTIME
from proxy.services.chat_inference_service import _model_connection_resolver
from proxy.services.chat_inference_service import _env_bool
from proxy.services.chat_inference_service import _runtime_from_provider_config
from proxy.services.chat_inference_service import _llm_runtime
from proxy.services.chat_inference_service import LlmRuntime
from proxy.services.chat_inference_service import _env_float
from proxy.services.chat_inference_service import _mlx_runtime
from proxy.services.chat_inference_service import model_connection_timeout
from proxy.services.chat_persistence_service import _history_success
from proxy.services.chat_persistence_service import _persist_recovered_stream_history
from proxy.services.chat_persistence_service import CHAT_HISTORY_EXTRA_COLUMNS
from proxy.services.chat_persistence_service import _json_text
from proxy.services.chat_persistence_service import ensure_chat_history_schema
from proxy.services.chat_persistence_service import save_chat_history
from proxy.services.chat_prompt_support import _query_route_payload
from proxy.services.chat_prompt_support import _parse_model_tool_calls
from proxy.services.chat_prompt_support import _compact_tool_result_for_prompt
from proxy.services.chat_prompt_support import _format_tool_results_for_model
from proxy.services.chat_prompt_support import _names_for_dataset_ids
from proxy.services.chat_prompt_support import _CJK_RE
from proxy.services.chat_prompt_support import _local_context_budget
from proxy.services.chat_prompt_support import _generation_token_budget
from proxy.services.chat_prompt_support import _dataset_sensitivities
from proxy.services.chat_prompt_support import source_excerpts
from proxy.services.chat_prompt_support import clean_visible_text
from proxy.services.chat_prompt_support import _extract_json_object
from proxy.services.chat_prompt_support import _dataset_ids_from_chunks
from proxy.services.chat_prompt_support import _dataset_name_map
from proxy.services.chat_prompt_support import _augment_model_tool_args
from proxy.services.chat_request_service import _run_chat_public
from proxy.services.chat_request_service import _version_stamp
from proxy.services.chat_request_service import _run_chat_with_provider
from proxy.services.chat_request_service import _prepare_notebook_reader_memory
from proxy.services.chat_request_service import _run_chat

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/api", tags=["chat"])


@router.get("/commands")
async def list_chat_commands(_user=Depends(require_user)):
    """Палитра /-команд для GUI (команда + ярлык + описание). W11.17."""
    from proxy.services.command_service import list_commands
    return {"commands": list_commands()}






_SMETA_ROW_UNITS_RE = re.compile(
    r"^(?:"
    r"м|м2|м²|м3|м³|мм|см|км|шт\.?|компл\.?|комплект|ед\.?|"
    r"т|кг|100\s*м|100\s*м2|100\s*м²|100\s*шт|100\s*отверстий"
    r")$",
    re.IGNORECASE,
)


def _idempotency_payload(req: chat_request_contracts.ChatRequest) -> dict[str, Any]:
    """Fingerprint provider selection without retaining the plaintext API key."""
    payload = req.model_dump(mode="json")
    provider_config = payload.get("provider_config")
    if isinstance(provider_config, dict):
        secret = str(provider_config.pop("api_key", ""))
        provider_config["api_key_sha256"] = hashlib.sha256(secret.encode("utf-8")).hexdigest() if secret else ""
    return payload


SOURCE_LOOKUP_MARKERS = (
    "где смотреть",
    "где посмотреть",
    "какие нормы",
    "какая норма",
    "какой норматив",
    "каким норматив",
    "какие норматив",
    "нормы регулиру",
    "нормы примен",
    "требования примен",
)


_THINK_RE = re.compile(r"<think>.*?</think>", re.DOTALL | re.IGNORECASE)


# ── Нативный ollama /api/chat с think:false (#1b) ──────────────────────────────────────────
# OpenAI-совместимый эндпоинт ollama ИГНОРИРУЕТ управление «думаньем» (think, /no_think,
# chat_template_kwargs — проверено на qwen3.5:9b), и reasoning-модель тратит весь лимит токенов
# на размышления → пустой/CoT-ответ. Нативный /api/chat с think:false даёт ЧИСТЫЙ content.
# Совпадает с интентом кода (в основном промпте уже есть /no_think «без скрытых рассуждений»).


def _sse_event(event: str, data: Any) -> str:
    """Кадр SSE: `event:` + одно `data:` с JSON-телом. Юникод не эскейпим —
    клиент читает UTF-8."""
    return f"event: {event}\ndata: {json.dumps(data, ensure_ascii=False)}\n\n"






def _recoverable_stream_payload(req: chat_request_contracts.ChatRequest, stream_state: dict[str, Any], err: BaseException) -> dict[str, Any] | None:
    """Return a final payload from an already useful SSE answer if the tail failed.

    Broad notebook/project answers can stream a good response for minutes and then hit
    provider timeout/retry plumbing before the final frame. In that case the visible
    streamed answer is the best operator artifact we have, so finish it as
    UNVALIDATED instead of sending a late reset/error that erases it in the UI.
    """
    text = chat_prompt_support.clean_visible_text(str(stream_state.get("text") or ""))
    if not text:
        return None
    sources_payload = stream_state.get("sources_payload")
    if not isinstance(sources_payload, dict):
        sources_payload = {}
    return {
        "answer": text,
        "crag_status": "UNVALIDATED",
        "partial": True,
        "completion_status": "interrupted",
        "blocker": {"code": "STREAM_INTERRUPTED", "action": "Ответ оборвался. Сохранён полученный фрагмент; повторите запрос для полного ответа."},
        "sources": sources_payload.get("sources") or [],
        "source_excerpts": sources_payload.get("source_excerpts") or [],
        "source_map": sources_payload.get("source_map") or [],
        "effective_dataset_filter": req.dataset_filter,
        "retrieval_trace": {
            "stream_recovery": {
                "reason": type(err).__name__,
                "detail": str(err)[:300],
                "tokens": stream_state.get("tokens", 0),
                "chars": len(text),
                "completion_status": "interrupted",
            }
        },
        "cache": "stream_recovered",
        "validation": {"enabled": False, "reason": "stream_recovered_after_partial_answer"},
    }


@router.post("/chat")
async def chat(
    req: chat_request_contracts.ChatRequest,
    _user=Depends(require_user),
    idempotency_key: Annotated[str | None, Header(alias="Idempotency-Key")] = None,
):
    """W5.1: нестриминговый эндпоинт — поведение неизменно (M5, смоуки, АРТЕЛЬ,
    chat_format_smoke). token_sink=None → путь stream:False, как раньше.

    Внешний клиент может передать ``Idempotency-Key``. Повтор с тем же телом
    получит исходный ответ без нового вызова модели; тот же ключ с другим телом
    отклоняется.
    """
    chat_request_contracts._require_candidate_acceptance(req, _user)
    if not idempotency_key:
        return decorate_payload(await chat_request_service._run_chat_public(req))

    from proxy.services.request_idempotency_service import (
        IdempotencyConflict,
        begin,
        caller_scope,
        complete,
        release,
        request_fingerprint,
    )

    caller = caller_scope(_user)
    fingerprint = request_fingerprint(_idempotency_payload(req))
    try:
        idem_state, cached = await asyncio.to_thread(
            begin,
            operation="chat",
            caller=caller,
            idempotency_key=idempotency_key,
            request_hash=fingerprint,
        )
    except (ValueError, IdempotencyConflict) as error:
        raise HTTPException(409, str(error)) from error
    if idem_state == "completed" and cached is not None:
        return cached
    if idem_state == "in_progress":
        raise HTTPException(
            409,
            "Запрос с этим Idempotency-Key уже выполняется",
            headers={"Retry-After": "2"},
        )

    try:
        result = decorate_payload(await chat_request_service._run_chat_public(req))
    except Exception:
        await asyncio.to_thread(
            release,
            operation="chat",
            caller=caller,
            idempotency_key=idempotency_key,
            request_hash=fingerprint,
        )
        raise
    try:
        await asyncio.to_thread(
            complete,
            operation="chat",
            caller=caller,
            idempotency_key=idempotency_key,
            request_hash=fingerprint,
            response=result,
        )
    except Exception as error:  # noqa: BLE001 - first caller still receives paid result
        logger.error("[IDEMPOTENCY] chat response persistence failed: %s", error)
    return result


@router.post("/chat/stream")
async def chat_stream(req: chat_request_contracts.ChatRequest, _user=Depends(require_user)):
    """W5.1: SSE-стриминг. События:
      • `token` — кусок ответа по мере генерации (только generic-LLM путь);
      • `progress` — видимый шаг workflow для tool/детерминированных веток;
      • `reset` — очистить накопленный текст (ретрай/деградация на MLX);
      • `final` — полный payload (sources + вердикт валидации в `crag_status`);
      • `error` — {status, detail}.
    Детерминированные/tool ветки не подделывают токены модели: они шлют progress,
    а затем авторитетный final payload."""
    if not req.question.strip():
        raise HTTPException(400, "Empty question")
    chat_request_contracts._require_candidate_acceptance(req, _user)
    queue: asyncio.Queue = asyncio.Queue()
    stream_state: dict[str, Any] = {
        "tokens": 0,
        "text": "",
        "sources_payload": {},
    }

    async def sink(ev: dict) -> None:
        event = ev.get("event")
        if event == "token" and ev.get("data"):
            stream_state["tokens"] += 1
            stream_state["text"] = str(stream_state.get("text") or "") + str(ev.get("data") or "")
        elif event == "reset":
            stream_state["tokens"] = 0
            stream_state["text"] = ""
        elif event == "sources" and isinstance(ev.get("data"), dict):
            stream_state["sources_payload"] = ev.get("data") or {}
        await queue.put(ev)

    async def runner() -> None:
        try:
            await sink({"event": "progress", "data": {"stage": "prepare", "label": "Подготавливаю запрос"}})
            result = decorate_payload(await chat_request_service._run_chat_with_provider(req, token_sink=sink))
            if stream_state["tokens"] == 0:
                answer_text = str(result.get("answer") or result.get("response") or "")
                if answer_text:
                    await sink({"event": "token", "data": answer_text})
            await queue.put({"event": "final", "data": result})
        except HTTPException as he:
            recovered = _recoverable_stream_payload(req, stream_state, he)
            if recovered is not None:
                recovered = chat_persistence_service._persist_recovered_stream_history(req, recovered)
                await queue.put({"event": "final", "data": decorate_payload(recovered)})
            else:
                public = public_error_payload(status_code=he.status_code, detail=he.detail)
                await queue.put({"event": "error", "data": {"status": he.status_code, **public}})
        except Exception as e:  # noqa: BLE001 — любую ошибку доносим клиенту как событие
            logger.error("[CHAT/STREAM] %s", e)
            recovered = _recoverable_stream_payload(req, stream_state, e)
            if recovered is not None:
                recovered = chat_persistence_service._persist_recovered_stream_history(req, recovered)
                await queue.put({"event": "final", "data": decorate_payload(recovered)})
            else:
                public = public_error_payload(status_code=500, detail=str(e))
                await queue.put({"event": "error", "data": {"status": 500, **public}})
        finally:
            await queue.put(None)

    async def event_source():
        task = asyncio.create_task(runner())
        try:
            while True:
                item = await queue.get()
                if item is None:
                    break
                yield _sse_event(item["event"], item.get("data", ""))
        finally:
            if not task.done():
                logger.info("[CHAT/STREAM] client disconnected; cancelling this request")
                task.cancel()
                await asyncio.gather(task, return_exceptions=True)

    return StreamingResponse(
        event_source(),
        media_type="text/event-stream",
        headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no", "Connection": "keep-alive"},
    )



from proxy.services.chat_runtime import _active_dispatcher_reindex_jobs
from proxy.services.chat_inference_service import chat_validation_enabled
