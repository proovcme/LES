"""Chat request scope, profile, memory and evidence orchestration."""
from __future__ import annotations
from proxy.services.operation_progress_service import chat_progress
import asyncio
import logging
import os
import time
from typing import Any
import httpx
from fastapi import HTTPException
from backend.runtime_paths import mutable_path
from proxy.services.chat_evidence_application_service import (
    EvidenceRequestContext,
    EvidenceRuntimeDeps,
    ResponseBoundary,
    run_chat_evidence_application,
)
from proxy.services.class_router_service import build_class_suggestions
from proxy.services.openai_compatible_transport_service import OpenAICompatibleTransport
from backend.inference.routing import is_cloud_provider
from proxy.services.context_expander_service import expand_context_windows
from proxy.services.context_memory_service import build_context_memory_block
from proxy.services.memory_service import session_memory, session_recent_retrieval_traces
from proxy.services.kot_service import analyze_question
from proxy.services.notebook_study_service import is_notebook_study_query
from proxy.services.dataset_memory_service import (
    get_typed_dataset_memory,
    run_dataset_reader_pass,
    schedule_dataset_reader_pass,
    select_topic_retrieval_plan,
)
from proxy.services.project_summary_service import is_project_inventory_query, resolve_inventory_file_reference
from proxy.services.query_router import route_query
from proxy.services.retrieval_service import resolve_dataset_ids, retrieve_chat_chunks
from proxy.services.runtime_admission import (
    GenerationSlotTimeout,
    count_active_jobs,
    evaluate_chat_admission,
)
from proxy.services.public_error_service import public_error_payload
from proxy.services.semantic_cache import (
    SemanticCache,
    dataset_scope_key,
    embed_question,
    semantic_cache_enabled,
    semantic_cache_threshold,
)
from proxy.services import chat_inference_service
from proxy.services import chat_persistence_service
from proxy.services import chat_prompt_support
from proxy.services import chat_request_contracts
from proxy.services import chat_runtime

logger = logging.getLogger(__name__)

async def _run_chat_with_provider(req: chat_request_contracts.ChatRequest, token_sink=None):
    from backend.product_edition import is_light
    if not is_light():
        return await _run_chat_bound(req, token_sink)
    from proxy.services.background_summary_service import foreground_request
    async with foreground_request(req.session_id):
        return await _run_chat_bound(req, token_sink)


async def _run_chat_bound(req: chat_request_contracts.ChatRequest, token_sink=None):
    """Bind a provider to this asyncio context only, then reliably remove it."""
    try:
        from backend.product_edition import is_light
        if is_light():
            from proxy.services.chat_profile_service import canonical_profile_mode
            try:
                canonical_profile_mode(req.mode)
            except ValueError as error:
                raise HTTPException(409, detail={
                    "code": "MODE_UNAVAILABLE_IN_LIGHT",
                    "detail": "Этот режим отсутствует в LES RAG. Выберите чат или поиск по документам.",
                }) from error
        if req.provider_config is None:
            if token_sink is None:
                return await _run_chat(req)
            return await _run_chat(req, token_sink=token_sink)
        if os.getenv("LES_DEMO_PROVIDER_OVERRIDE_ENABLED", "false").strip().lower() not in {
            "1",
            "true",
            "yes",
            "on",
        }:
            raise HTTPException(409, "SESSION_PROVIDER_OVERRIDE_DISABLED")
        runtime = chat_inference_service._runtime_from_provider_config(req.provider_config)
        runtime_token = chat_inference_service._REQUEST_LLM_RUNTIME.set(runtime)
        consent_token = chat_inference_service._REQUEST_CLOUD_CONSENT.set(is_cloud_provider(runtime.provider))
        try:
            if token_sink is None:
                return await _run_chat(req)
            return await _run_chat(req, token_sink=token_sink)
        finally:
            chat_inference_service._REQUEST_CLOUD_CONSENT.reset(consent_token)
            chat_inference_service._REQUEST_LLM_RUNTIME.reset(runtime_token)
    except GenerationSlotTimeout as error:
        logger.info("[CHAT] model queue timeout: %s", error)
        detail = public_error_payload(
            status_code=429,
            detail={
                "code": error.code,
                "detail": "Модель занята. Запрос дождался своей очереди, но время ожидания истекло.",
            },
        )
        raise HTTPException(429, detail=detail) from error
    except HTTPException as error:
        detail = public_error_payload(status_code=error.status_code, detail=error.detail)
        raise HTTPException(error.status_code, detail=detail, headers=error.headers) from error


async def _run_chat_public(req: chat_request_contracts.ChatRequest):
    """Non-stream boundary: preserve diagnostics in logs, never in HTTP JSON."""

    try:
        return await _run_chat_with_provider(req)
    except HTTPException:
        raise
    except Exception as error:
        logger.exception("[CHAT] unexpected request failure")
        detail = public_error_payload(status_code=500, detail=str(error))
        raise HTTPException(500, detail=detail) from error


async def _prepare_notebook_reader_memory(dataset_ids: list[str]) -> dict[str, Any]:
    """Best-effort model reader-pass before broad dataset study.

    Reader output is navigation only. It helps the final model choose files and
    sections, but the answer still needs retrieved chunks/tables as evidence.
    """
    # The reader pass is an additional LLM job, not retrieval.  Running it by
    # default made a broad chat silently start a second local model generation
    # and, after timeout, schedule it again in background.  Typed notebook
    # memory + RRF remain available; explicit warmup can opt this job back in.
    if not dataset_ids or not chat_inference_service._env_bool("LES_NOTEBOOK_READER_ON_STUDY", False):
        return {"schema": "dataset_reader_prepare_v1", "status": "disabled", "datasets": []}
    limit = chat_inference_service._env_int("LES_NOTEBOOK_READER_ON_STUDY_LIMIT", 2)
    timeout_s = chat_inference_service._env_float("LES_NOTEBOOK_READER_ON_STUDY_TIMEOUT", 35.0)
    prepared: list[dict[str, Any]] = []
    for dataset_id in [str(d) for d in dataset_ids if str(d).strip()][:limit]:
        try:
            memory = await asyncio.to_thread(get_typed_dataset_memory, dataset_id)
            if memory.get("reader_status") == "model":
                prepared.append({"dataset_id": dataset_id, "status": "ready"})
                continue
            try:
                updated = await asyncio.wait_for(
                    run_dataset_reader_pass(dataset_id, force=False),
                    timeout=timeout_s,
                )
                prepared.append({
                    "dataset_id": dataset_id,
                    "status": str(updated.get("reader_status") or "unknown"),
                })
            except TimeoutError:
                scheduled = schedule_dataset_reader_pass(
                    dataset_id,
                    reason="notebook_study_timeout",
                    force=False,
                    require_enabled=False,
                )
                prepared.append({
                    "dataset_id": dataset_id,
                    "status": "scheduled_after_timeout",
                    "scheduled": scheduled,
                })
        except Exception as err:  # noqa: BLE001
            logger.warning("[DATASET_READER] prepare skipped dataset=%s: %s", dataset_id, err)
            prepared.append({
                "dataset_id": dataset_id,
                "status": "skipped",
                "error": f"{type(err).__name__}: {err}",
            })
    return {"schema": "dataset_reader_prepare_v1", "status": "ok", "datasets": prepared}


def _version_stamp() -> dict:
    """Version-stamp для воспроизводимости (Codex §15, пет-размер): через месяц объяснить,
    почему тот же запрос дал другой ответ. v0.19: + version_info (app/harness/commit/флаги) из
    единого version_service — баг-репорт идентифицирует точный build."""
    stamp = {
        # Filled from the connection that actually completed the request.
        "llm_provider": "unknown",
        "llm_model": "unknown",
        "embed_model": os.getenv("EMBED_MODEL", "?"),
        "collection": os.getenv("RAG_COLLECTION", "") or "default",
        "prompt": "sys_normal_v1",
        "profiles": "v1",
    }
    try:
        from proxy.services.version_service import version_info_trace
        stamp["version_info"] = version_info_trace()
    except Exception:  # noqa: BLE001
        pass
    return stamp


async def _run_chat(req: chat_request_contracts.ChatRequest, token_sink=None):
    """Ядро чата. token_sink=None — обычный ответ (dict). Если задан — корутина
    `await token_sink({"event":..., "data":...})` получает события стриминга по
    мере генерации; итог всё равно возвращается dict'ом (его шлёт `chat_stream`
    финальным событием)."""
    from backend.product_edition import is_light
    if req.attachment_id and is_light():
        from proxy.services.chat_attachment_service import resolve_read_attachment
        try:
            await asyncio.to_thread(resolve_read_attachment, req.attachment_id)
        except FileNotFoundError as error:
            raise HTTPException(404, detail={
                "code": "CHAT_ATTACHMENT_NOT_FOUND",
                "message": "Вложение не найдено или срок его хранения истёк. Прикрепите файл заново.",
            }) from error
        except ValueError as error:
            raise HTTPException(422, detail={
                "code": "CHAT_ATTACHMENT_INVALID",
                "message": "Не удалось проверить целостность вложения. Прикрепите файл заново.",
            }) from error
    state = chat_runtime.get_chat_state()
    if not req.question.strip():
        raise HTTPException(400, "Empty question")
    t_request_start = time.time()

    pid = req.project_id or 0

    # v0.21: нормализованная ОБЛАСТЬ ПОИСКА (snapshot для trace/истории; явный ui-scope управляет ретривом).
    from proxy.services.scope_service import (
        document_grounding_enabled,
        explicit_dataset_filter,
        resolve_scope,
    )
    _scope_snap = resolve_scope(scope=req.scope, project_id=req.project_id,
                                dataset_ids=req.dataset_ids, dataset_filter=req.dataset_filter)
    grounding_enabled = document_grounding_enabled(
        _scope_snap["scope_type"], req.dataset_ids
    )
    if isinstance(req.scope, dict) and req.scope.get("scope_type"):
        # явный scope из ScopeSelector приоритетнее legacy: проставляем resolved в поля, которые
        # понимает существующий конвейер (без молчаливого fallback на «весь RAG»).
        if _scope_snap["resolved_dataset_ids"]:
            req.dataset_ids = _scope_snap["resolved_dataset_ids"]
        if _scope_snap["scope_type"] == "project" and _scope_snap["project_ids"]:
            req.project_id = _scope_snap["project_ids"][0]
            pid = req.project_id

    # Resolve one persistent profile snapshot before any professional route.
    from proxy.services.chat_profile_service import (
        resolve_chat_profile,
        resolve_profile_system_dataset_ids,
    )

    try:
        _profile_snapshot = resolve_chat_profile(
            session_id=req.session_id,
            requested_mode=req.mode,
            requested_revision_id=req.profile_revision_id,
            apply_revision=bool(req.apply_profile_revision),
        )
    except ValueError as error:
        raise HTTPException(409, f"Профиль чата не применён: {error}") from error
    req.mode = str(_profile_snapshot.get("mode") or "agent")

    # ── МАРШРУТИЗАЦИЯ ЧЕРЕЗ ProfileResolver (Codex §10.1A: единый контракт) ──
    # Все источники выбора пути сводятся к ОДНОЙ ProfileResolution. Явный режим → профиль,
    # а фактически выбранный канал уточняет резолюцию через refine. Так «какой канал дёрнут» — один записанный контракт
    # (query_route.profile), а не неявный control-flow. Резолвер сам не отвечает (§10.3 №4).
    from proxy.services.profile_resolver import (
        resolve as _resolve_profile, route_source_for_channel)
    _resolution = _resolve_profile(mode=req.mode, question=req.question)
    _PROFILE = _resolution.profile_id
    # Model-final-only invariant: свободный запрос не может завершиться ответом
    # regex/SQL/Python обработчика. Код читает, ищет, считает и проверяет внутри
    # evidence/tool loop; видимый ответ формулирует модель.
    def _profile_route(channel: str, operation: str | None, *,
                       base: dict | None = None, source: str | None = None) -> dict:
        """query_route c честным profile-трейсом: refine резолюции выбранным каналом + as_trace.
        Профиль не меняется — фиксируем КАК принят маршрут и КАКОЙ канал."""
        _resolution.refine(route_source=(source or route_source_for_channel(channel)),
                            channel=channel, operation=operation)
        route = dict(base or {})
        route["channel"] = channel
        if operation is not None:
            route["operation"] = operation
        route["profile"] = _resolution.as_trace()
        route["profile_snapshot"] = {
            key: _profile_snapshot.get(key)
            for key in (
                "revision_id", "mode", "name", "revision_no", "prompt_revision_id",
                "prompt_sha256", "skill_revision_id", "skill_sha256", "tools",
            )
        }
        return route

    # W11.17: /-команды (палитра). rewrite → переформулировать и пройти конвейером; иначе — детерм. ответ.
    from proxy.services.command_service import handle_command, is_command
    if is_command(req.question):
        cmd_res = handle_command(req.question, project_id=pid)
        if cmd_res and cmd_res.get("rewrite"):
            req.question = cmd_res["rewrite"]
        elif cmd_res is not None:
            cmd_payload = dict(cmd_res.get("command") or {})
            return {
                "answer": cmd_res["answer"],
                "crag_status": "DETERMINISTIC",
                "sources": [],
                "query_route": _profile_route("command", (cmd_res.get("command") or {}).get("action")),
                "validation": {"enabled": False, "reason": "deterministic_command"},
                "command": cmd_payload,
            }

    # Операторские заметки не создаются, не читаются и не подмешиваются в чат.
    # Контекст ниже содержит только явное вложение, LES.md, typed dataset passport
    # и историю текущей сессии, которую читает модель.
    memory_block = ""
    if req.attachment_context:
        attachment_block = (
            "Контекст прикреплённого файла (read-mode, не индекс):\n"
            f"{req.attachment_context}"
        )
        memory_block = attachment_block + ("\n\n" + memory_block if memory_block else "")
    # LES.md: контекст папки/проекта — ВСЕГДА (как CLAUDE.md для harness). Симметрия датасет↔проект
    # (#2): если выбран ДАТАСЕТ без проекта (pid=0), резолвим его объект и подмешиваем тот же LES.md,
    # что и в режиме проекта — иначе режим датасета терял контекст (системы/стадия/состав папки).
    from proxy.services.typed_memory_projection_service import resolve_session_memory_scope
    _les_pid, _workspace_registered = resolve_session_memory_scope(req.session_id, pid)
    if not _workspace_registered and not _les_pid and req.dataset_ids:
        try:
            from proxy.services.project_service import project_for_dataset
            _les_pid = project_for_dataset(req.dataset_ids[0]) or 0
        except Exception:  # noqa: BLE001
            _les_pid = 0
    if _les_pid:
        try:
            from proxy.services.les_md_service import context_for_chat
            les_md_block = context_for_chat(_les_pid)
            if les_md_block:
                memory_block = les_md_block + ("\n\n" + memory_block if memory_block else "")
                logger.info("[LES.md] подмешан контекст объекта #%s (%s симв.; scope=%s)",
                            _les_pid, len(les_md_block), "project" if pid else "dataset")
        except Exception as err:  # noqa: BLE001
            logger.warning("[LES.md] context inject failed: %s", err)
    if memory_block:
        logger.info("[MEMORY] подмешано %s символов рабочей памяти", len(memory_block))
    # «Запоминать всё»: история диалога текущей сессии в промпт (чат потурно безсостоятельный).
    # Только в промпт LLM, НЕ дописываем к детерминированным ответам (это были бы простыни).
    prior_traces: list[dict[str, Any]] = []
    try:
        session_block = session_memory(req.session_id)
    except Exception as err:
        logger.warning("[MEMORY] session recall failed: %s", err)
        session_block = ""
    try:
        from proxy.services.chat_evidence_manifest_service import (
            compact_prior_evidence_index,
            format_prior_evidence_index,
        )

        prior_traces = session_recent_retrieval_traces(req.session_id, max_turns=6)
        prior_manifests = [
            trace.get("evidence_manifest")
            for trace in prior_traces
            if isinstance(trace, dict) and isinstance(trace.get("evidence_manifest"), dict)
        ]
        prior_index = format_prior_evidence_index(
            compact_prior_evidence_index(prior_manifests, max_items=24)
        )
        if prior_index:
            session_block = "\n\n".join(part for part in (session_block, prior_index) if part)
    except Exception as err:  # advisory continuity must not block a new question
        logger.warning("[MEMORY] prior evidence index skipped: %s", err)
    from proxy.services.chat_capability_scope_service import resolve_selected_sources_only

    req.selected_sources_only = resolve_selected_sources_only(
        req.selected_sources_only,
        prior_traces,
    )

    rag_backend = state.backend

    # W17.1: двойной режим. Если задан project_id и пользователь не выбрал датасеты
    # явно — сужаем ретрив к датасетам объекта (режим проекта). Нет project_id или
    # нет привязанных датасетов → обычный RAG (поведение неизменно). Явный выбор
    # пользователя приоритетнее проекта.
    effective_dataset_ids = req.dataset_ids
    if req.project_id and not req.dataset_ids:
        try:
            from proxy.services.project_service import project_dataset_ids
            scope = await asyncio.to_thread(project_dataset_ids, req.project_id)
            if scope:
                effective_dataset_ids = scope
                logger.info("[PROJECT] режим объекта %s → датасеты %s", req.project_id, scope)
        except Exception as proj_err:
            logger.warning("[PROJECT] scope resolve failed: %s", proj_err)

    profile_dataset_ids = resolve_profile_system_dataset_ids(
        _profile_snapshot,
        current_dataset_ids=effective_dataset_ids,
    )
    profile_bound_system_datasets = profile_dataset_ids != list(
        effective_dataset_ids or []
    )
    if profile_bound_system_datasets:
        effective_dataset_ids = profile_dataset_ids
        grounding_enabled = True
        logger.info(
            "[PROFILE] revision %s bound system datasets %s",
            _profile_snapshot.get("revision_id"),
            profile_dataset_ids,
        )

    query_intent = route_query(
        req.question,
        dataset_filter=req.dataset_filter,
        dataset_ids=effective_dataset_ids,
    )
    kot_decision = analyze_question(req.question)
    effective_dataset_filter = explicit_dataset_filter(
        req.dataset_filter,
        grounding_enabled=grounding_enabled,
    )
    logger.info(
        "[QUERY_ROUTER] channel=%s reason=%s filter=%s",
        query_intent.channel,
        query_intent.reason,
        effective_dataset_filter,
    )
    # ADR-12: мультикласс через диалог — чипы-варианты для прочих распознанных классов.
    # (retrieval_trace тут ещё не инициализирован — пишем класс-метки в трейс ниже, после retrieve.)
    class_suggestions = build_class_suggestions(req.question, primary_filter=effective_dataset_filter)

    if not grounding_enabled:
        scope_source = "none"
    elif req.dataset_ids:
        scope_source = "explicit_dataset_ids"
    elif req.dataset_filter:
        scope_source = "explicit_dataset_filter"
    elif req.project_id:
        scope_source = "explicit_project"
    elif profile_bound_system_datasets:
        scope_source = "profile_system_datasets"
    elif effective_dataset_filter:
        scope_source = "inferred_filter"
    else:
        scope_source = "all_corpus"
    scope_resolution: dict[str, Any] = {}
    if grounding_enabled:
        _dataset_ids = await resolve_dataset_ids(
            rag_backend,
            effective_dataset_ids,
            effective_dataset_filter,
            logger,
            question=req.question,
            resolution_trace=scope_resolution,
            scope_source=scope_source,
        )
        dataset_name_by_id = await chat_prompt_support._dataset_name_map(rag_backend)
    else:
        _dataset_ids = []
        dataset_name_by_id = {}
        scope_resolution.update({
            "scope_source": "none",
            "scope_type": "none",
            "document_grounding_enabled": False,
            "status": "skipped",
        })
    scope_resolution.setdefault("scope_type", _scope_snap["scope_type"])
    scope_resolution.setdefault("document_grounding_enabled", grounding_enabled)
    resolved_dataset_names = chat_prompt_support._names_for_dataset_ids(_dataset_ids, dataset_name_by_id)
    target_file_ref: dict[str, Any] | None = None
    target_file_refs: list[dict[str, Any]] = []
    target_doc_filter: list[str] = []
    if _dataset_ids:
        explicit_targets = list(req.target_files or [])
        if req.target_file:
            explicit_targets.insert(0, req.target_file)
        target_queries = list(dict.fromkeys(explicit_targets)) or [req.question]
        for target_query in target_queries:
            resolved_ref = await asyncio.to_thread(
                resolve_inventory_file_reference,
                target_query,
                [str(d) for d in _dataset_ids],
            )
            if not resolved_ref:
                continue
            target_file_refs.append(resolved_ref)
            if resolved_ref.get("match_status") == "matched" and resolved_ref.get("file_name"):
                target_doc_filter.append(str(resolved_ref["file_name"]))
            elif resolved_ref.get("match_status") == "ambiguous":
                logger.info("[FILE_TARGET] ambiguous file reference: %s", resolved_ref.get("match_count"))
        target_doc_filter = list(dict.fromkeys(target_doc_filter))
        if len(target_file_refs) == 1:
            target_file_ref = target_file_refs[0]
        if target_doc_filter:
            logger.info("[FILE_TARGET] question scoped to %s explicit files", len(target_doc_filter))
    try:
        context_memory_block = build_context_memory_block(
            session_id=req.session_id,
            dataset_ids=_dataset_ids,
            dataset_names=resolved_dataset_names,
            storage_root=mutable_path("./storage/datasets"),
            # Typed dataset memory is added once by the evidence application.
            # Rebuilding the deep dataset profile here duplicated navigation and
            # cost 30-40 seconds on BAI before retrieval even started.  Keep only
            # the cheap chat-session passport in this layer.
            max_datasets=0,
        )
        if context_memory_block:
            memory_block = memory_block + ("\n\n" if memory_block else "") + context_memory_block
            logger.info("[CONTEXT_MEMORY] подмешан паспорт чата/датасетов (%s симв.)", len(context_memory_block))
    except Exception as err:  # навигационная память не должна блокировать RAG
        logger.warning("[CONTEXT_MEMORY] prompt block skipped: %s", err)

    # W11.15 used to auto-hijack broad chat questions ("расскажи про проект") into a
    # deterministic project register. That made LES look like a file inventory instead of a
    # notebook/RAG synthesis. Project summary stays available as an explicit command/MCP tool,
    # but normal chat questions now continue into retrieval + model.

    query_route_payload = chat_prompt_support._query_route_payload(query_intent, effective_dataset_filter, kot_decision)
    query_route_payload["scope"] = _scope_snap   # v0.21: где реально искали (snapshot для trace/истории)
    if target_file_ref:
        query_route_payload["target_file"] = target_file_ref
    if target_file_refs:
        query_route_payload["target_files"] = target_file_refs
    study_requested = bool(req.dataset_ids or effective_dataset_filter) and is_notebook_study_query(req.question)
    inventory_requested = bool(req.dataset_ids or effective_dataset_filter) and is_project_inventory_query(req.question)
    if study_requested:
        query_route_payload["breadth"] = "wide"
        query_route_payload["notebook_study_requested"] = True
    if inventory_requested:
        query_route_payload["inventory_requested"] = True
    topic_retrieval_plan: dict[str, Any] = {}
    topic_doc_filter: list[str] = []
    if _dataset_ids and not target_doc_filter:
        try:
            topic_memories = await asyncio.to_thread(
                lambda: [get_typed_dataset_memory(str(dataset_id)) for dataset_id in _dataset_ids]
            )
            topic_retrieval_plan = await asyncio.to_thread(
                select_topic_retrieval_plan,
                topic_memories,
                req.question,
            )
            topic_doc_filter = [
                str(item.get("file_name") or "")
                for item in (topic_retrieval_plan.get("selected_files") or [])
                if str(item.get("file_name") or "").strip()
            ]
            topic_doc_filter = list(dict.fromkeys(topic_doc_filter))
            if topic_doc_filter:
                query_route_payload["topic_selection"] = {
                    "schema": topic_retrieval_plan.get("schema"),
                    "selected_topics": topic_retrieval_plan.get("selected_topics") or [],
                    "selected_files": topic_retrieval_plan.get("selected_files") or [],
                    "selected_sections": topic_retrieval_plan.get("selected_sections") or [],
                    "fallback": topic_retrieval_plan.get("fallback"),
                }
        except Exception as topic_err:  # noqa: BLE001
            logger.warning("[TOPIC_RETRIEVAL] topic selection skipped: %s", topic_err)
            topic_retrieval_plan = {
                "schema": "dataset_topic_selection_v1",
                "status": "skipped",
                "error": f"{type(topic_err).__name__}: {topic_err}",
            }
    # #2: финальный resolved-канал = семантический RAG. default_rag (ни команда/regex/каскад
    # не поймали) → честный fallback; иначе keyword (route_query поймал по словарю). profile-
    # трейс кладём в payload — как у детерминированных каналов выше: один контракт в каждом route.
    if _resolution.route_source == "explicit_mode":
        _resolution.channel = query_intent.channel
        _resolution.operation = query_intent.reason
    else:
        _resolution.refine(
            route_source=("fallback" if query_intent.reason == "default_rag" else "keyword"),
            channel=query_intent.channel,
            operation=query_intent.reason,
        )
    query_route_payload["profile"] = _resolution.as_trace()
    query_route_payload["profile_snapshot"] = {
        key: _profile_snapshot.get(key)
        for key in (
            "revision_id", "mode", "name", "revision_no", "prompt_revision_id",
            "prompt_sha256", "skill_revision_id", "skill_sha256", "tools",
        )
    }
    cache = SemanticCache()
    cache_embedding = None
    cache_scope = ""
    cache_marker = "miss"

    use_semantic_cache = (
        req.semantic_cache_enabled
        if req.semantic_cache_enabled is not None
        else semantic_cache_enabled()
    )
    if grounding_enabled:
        use_semantic_cache = False
    if study_requested or inventory_requested or target_doc_filter or topic_doc_filter:
        # Broad project/object questions must re-read the selected area. A cached short RAG table
        # turns "расскажи про объект" into a stale narrow answer and hides the broad reading layer.
        # File-register questions need fresh MetaDB inventory, not an old aggregate RAG answer.
        # File-target questions must stay scoped to the named document.
        # Topic-guided retrieval must not be bypassed by a previous flat semantic-cache answer.
        use_semantic_cache = False
    use_validation = (
        req.validation_enabled
        if req.validation_enabled is not None
        else chat_inference_service.chat_validation_enabled()
    )
    validation_skip_reason = ""
    if req.validation_enabled is None and (study_requested or inventory_requested):
        # Broad project/inventory answers are grounded by source-map plus deterministic
        # MetaDB inventory/artifact. Running TOSKA over the full synthesized report added
        # 30-40s on BAI while not improving the operator-facing evidence boundary.
        use_validation = False
        validation_skip_reason = "broad_project_inventory_fast_path"
        query_route_payload["validation_policy"] = {
            "enabled": False,
            "reason": validation_skip_reason,
            "evidence": "source_map+project_inventory_artifact",
        }

    table_result = None

    from proxy.services.runtime_admission import live_memory_metrics
    fresh_metrics = await asyncio.to_thread(live_memory_metrics, state.metrics_cache)
    admission = evaluate_chat_admission(
        current_mode=state.current_mode,
        metrics_cache=fresh_metrics,
        active_jobs=count_active_jobs(state.job_service, state.job_tracker) + chat_runtime._active_dispatcher_reindex_jobs(state),
    )
    if not admission.allowed:
        logger.info("[CHAT] admission blocked: %s", admission.reason)
        if any(item.startswith(("ram_free_gb=", "swap_pct=")) for item in admission.failures):
            detail = {"code": "CHAT_MEMORY_PRESSURE", "detail":
                      "Сейчас недостаточно свободной памяти для ответа. Закройте ненужные приложения "
                      "или дождитесь завершения обработки документов и повторите запрос."}
        elif admission.active_jobs:
            detail = {"code": "CHAT_INDEXING_BUSY", "detail":
                      "Сейчас обрабатываются документы. Дождитесь завершения обработки "
                      "или остановите очередь в разделе «Данные» и повторите запрос."}
        else:
            detail = {"code": "CHAT_GENERATION_PAUSED", "detail":
                      "Ответы временно приостановлены настройками ресурсов. "
                      "Проверьте режим работы в настройках и повторите запрос."}
        raise HTTPException(status_code=admission.status_code, detail=detail)

    if is_light() and req.session_id:
        session_block = session_memory(req.session_id)

    if use_semantic_cache:
        try:
            datasets = await rag_backend.list_datasets()
            cache_scope = dataset_scope_key(datasets, _dataset_ids)
            cache_embedding = await embed_question(rag_backend, req.question)
            if cache_embedding:
                cache_hit = cache.lookup(
                    req.question,
                    cache_scope,
                    cache_embedding,
                    semantic_cache_threshold(),
                )
                if cache_hit:
                    cache_trace = {
                        "mode": "cache",
                        "vector_count": 0,
                        "lexical_count": 0,
                        "merged_count": 0,
                        "retry_count": 0,
                        "quality_status": "cache_hit",
                    }
                    history_id = None
                    state.crag_stats["verified"] += 1
                    state.chat_metrics["latency_search"].append(0.0)
                    state.chat_metrics["latency_gen"].append(0.0)
                    state.chat_metrics["tokens"].append(0)
                    state.chat_metrics["crag_pass"] += 1
                    for key in ("latency_search", "latency_gen", "tokens"):
                        state.chat_metrics[key] = state.chat_metrics[key][-100:]
                    try:
                        history_id = chat_persistence_service.save_chat_history(
                            question=req.question,
                            answer=cache_hit.answer,
                            sources=cache_hit.sources,
                            crag_status="VERIFIED",
                            latency_sec=0.0,
                            tokens=0,
                            session_id=req.session_id,
                            requested_dataset_filter=req.dataset_filter,
                            effective_dataset_filter=effective_dataset_filter,
                            resolved_dataset_ids=_dataset_ids,
                            resolved_dataset_names=resolved_dataset_names,
                            source_dataset_ids=_dataset_ids,
                            source_dataset_names=resolved_dataset_names,
                            query_route=query_route_payload,
                            retrieval_trace=cache_trace,
                            cache_type=cache_hit.cache_type,
                            validation_enabled=use_validation,
                            success=1,
                        )
                    except Exception as db_err:
                        logger.warning("[CHAT] History save error: %s", db_err)
                    logger.info("[SEM_CACHE] hit similarity=%.3f", cache_hit.similarity)
                    state.chat_metrics["cache_hit"] = state.chat_metrics.get("cache_hit", 0) + 1
                    return {
                        "answer": cache_hit.answer,
                        "crag_status": "VERIFIED",
                        "sources": cache_hit.sources,
                        "effective_dataset_filter": effective_dataset_filter,
                        "query_route": query_route_payload,
                        "retrieval_trace": cache_trace,
                        "cache": cache_hit.cache_type,
                        "similarity": round(cache_hit.similarity, 3),
                        "history_id": history_id,
                    }
        except Exception as cache_err:
            logger.warning("[SEM_CACHE] lookup skipped: %s", cache_err)

    evidence_request = EvidenceRequestContext(
        req=req,
        dataset_ids=_dataset_ids,
        scope_resolution=scope_resolution,
        effective_dataset_filter=effective_dataset_filter,
        resolved_dataset_names=resolved_dataset_names,
        dataset_name_by_id=dataset_name_by_id,
        query_route_payload=query_route_payload,
        target_doc_filter=target_doc_filter,
        target_file_ref=target_file_ref,
        topic_doc_filter=topic_doc_filter,
        topic_retrieval_plan=topic_retrieval_plan,
        inventory_requested=inventory_requested,
        study_requested=study_requested,
        memory_block=memory_block,
        session_block=session_block,
        class_suggestions=class_suggestions,
        use_semantic_cache=use_semantic_cache,
        use_validation=use_validation,
        validation_skip_reason=validation_skip_reason,
        route=query_intent,
        table_result=table_result,
        request_started_at=t_request_start,
        profile_snapshot=_profile_snapshot,
    )
    evidence_runtime = EvidenceRuntimeDeps(
        state=state,
        rag_backend=rag_backend,
        cache=cache,
        cache_embedding=cache_embedding,
        cache_marker=cache_marker,
        cache_scope=cache_scope,
        augment_model_tool_args=chat_prompt_support._augment_model_tool_args,
        compact_tool_result_for_prompt=chat_prompt_support._compact_tool_result_for_prompt,
        dataset_ids_from_chunks=chat_prompt_support._dataset_ids_from_chunks,
        dataset_sensitivities=chat_prompt_support._dataset_sensitivities,
        env_bool=chat_inference_service._env_bool,
        env_float=chat_inference_service._env_float,
        env_int=chat_inference_service._env_int,
        expand_context_windows=expand_context_windows,
        format_tool_results_for_model=chat_prompt_support._format_tool_results_for_model,
        generation_token_budget=chat_prompt_support._generation_token_budget,
        local_context_budget=chat_prompt_support._local_context_budget,
        names_for_dataset_ids=chat_prompt_support._names_for_dataset_ids,
        parse_model_tool_calls=chat_prompt_support._parse_model_tool_calls,
        prepare_notebook_reader_memory=_prepare_notebook_reader_memory,
        record_cloud_cost=chat_inference_service._record_cloud_cost,
        retrieve_chat_chunks=retrieve_chat_chunks,
        source_excerpts=chat_prompt_support.source_excerpts,
        model_connection_resolver=chat_inference_service._model_connection_resolver,
        model_connection_transport=lambda client, secret_store: OpenAICompatibleTransport(
            client=client,
            secret_store=secret_store,
            timeout=chat_inference_service.model_connection_timeout(),
        ),
    )
    response_boundary = ResponseBoundary(
        save_chat_history=chat_persistence_service.save_chat_history,
        token_sink=token_sink,
        version_stamp=_version_stamp,
    )
    async with httpx.AsyncClient(timeout=30.0, follow_redirects=False) as capability_client:
        await chat_progress(token_sink, "connection", "Проверяю подключение модели")
        await chat_inference_service._refresh_stale_bound_model_capabilities(capability_client)
    return await run_chat_evidence_application(
        evidence_request,
        evidence_runtime,
        response_boundary,
    )
