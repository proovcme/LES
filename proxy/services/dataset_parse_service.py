"""Admission, progress and sequential document parsing."""
from __future__ import annotations
import asyncio
import logging
from datetime import datetime
from pathlib import Path
from typing import Any
from fastapi import HTTPException
from proxy.services.dataset_memory_service import schedule_dataset_reader_pass
from proxy.services.resource_governor import active_parse_priority_order, current_runtime_profile
from proxy.services.runtime_admission import evaluate_memory_pressure

from proxy.services.dataset_contracts import (ACTIVE_PARSE_SCHEDULER_STATUSES, DEFAULT_PARSE_DRAIN_MAX_BATCHES, PARSE_MAX_SWAP_PCT, PARSE_MIN_FREE_GB, PARSE_POST_MAX_SWAP_PCT, ParseSchedulerRequest, _PARSE_STAGE_LABELS)
from proxy.services.dataset_runtime import (DatasetRouterState)



logger = logging.getLogger(__name__)


async def parse_memory_state() -> dict[str, Any]:
    import psutil
    try:
        ram = psutil.virtual_memory()
        swap = psutil.swap_memory()
    except (OSError, RuntimeError) as error:
        raise HTTPException(503, "Не удалось проверить доступную память Windows. Индексация приостановлена; повторите проверку.") from error
    memory = {"ram_free_gb": ram.available / (1024 ** 3), "swap_pct": swap.percent, "swap_used_gb": swap.used / (1024 ** 3)}
    return {**memory, "state": evaluate_memory_pressure(memory).state, "raw": memory, "source": "operating_system"}


async def assert_parse_admission(
    state: DatasetRouterState,
    *,
    min_free_gb: float = PARSE_MIN_FREE_GB,
    max_swap_pct: float = PARSE_MAX_SWAP_PCT,
) -> None:
    from backend.product_edition import is_light
    if is_light():
        from proxy.services.model_connection_resolver_service import ModelConnectionResolutionError
        embedder = getattr(state.backend, "embed_parse", None)
        if embedder is None or embedder.connection_mode != "active":
            raise HTTPException(409, "Для поиска по документам назначьте модель эмбеддингов в настройках моделей.")
        try:
            await asyncio.to_thread(embedder._resolve_embedding_connection)
        except ModelConnectionResolutionError as error:
            raise HTTPException(409, "Модель для поиска не назначена или не готова. Откройте Настройки → Модели, проверьте подключение и назначьте его для поиска по документам.") from error
    try:
        qdrant_ok = await state.backend.health()
    except Exception as error:
        raise HTTPException(status_code=503, detail=f"Qdrant health failed: {error}") from error
    if not qdrant_ok:
        raise HTTPException(status_code=503, detail="Qdrant is not healthy")

    memory = await parse_memory_state()
    ram_free_gb = memory["ram_free_gb"]
    swap_pct = memory["swap_pct"]
    # На macOS swap_pct почти ВСЕГДА высок (висячие stale-аллокации) даже при куче свободной RAM —
    # это НЕ давление памяти. Надёжный сигнал = свободная RAM. Блокируем при низкой RAM, либо при
    # ЭКСТРЕМАЛЬНОМ swap-thrashing (>90%) одновременно с уже подсевшей RAM. max_swap_pct (дефолт под
    # не-macOS, 45%) для блокировки НЕ используем — иначе нормальный macOS-swap ложно режет индексацию.
    from backend.parse_admission import parse_memory_block_reason
    block_reason = parse_memory_block_reason(ram_free_gb, swap_pct, min_free_gb)
    if block_reason:
        raise HTTPException(status_code=429, detail=block_reason)




def _priority_rank(dataset_name: str, priority_order: list[str]) -> int:
    try:
        return priority_order.index(dataset_name)
    except ValueError:
        return len(priority_order)


async def pending_parse_datasets(
    state: DatasetRouterState,
    priority_order: list[str] | None = None,
) -> list[dict[str, Any]]:
    if not hasattr(state.backend, "health_snapshot"):
        return []
    snapshot = await state.backend.health_snapshot()
    items = []
    for dataset in snapshot.get("datasets", []):
        pending = int(dataset.get("pending_files") or 0)
        if pending > 0:
            items.append(
                {
                    "dataset_id": dataset["id"],
                    "dataset_name": dataset.get("name") or dataset["id"],
                    "pending_files": pending,
                }
            )
    priority = priority_order or active_parse_priority_order(state.current_mode)
    return sorted(
        items,
        key=lambda item: (
            _priority_rank(item["dataset_name"], priority),
            -item["pending_files"],
            item["dataset_name"],
        ),
    )


def active_parse_scheduler_job(state: DatasetRouterState) -> tuple[str, dict[str, Any]] | None:
    for job_id, job in state.job_tracker.items():
        status = str(job.get("status", "")).upper()
        if status not in ACTIVE_PARSE_SCHEDULER_STATUSES:
            continue
        message = str(job.get("message", ""))
        is_scheduler = (
            job.get("type") == "rag_parse_scheduler"
            or "Parse scheduler" in message
            or message.startswith("Batch ")
        )
        if is_scheduler:
            return job_id, job
    return None


async def _dataset_name_for_id(state: DatasetRouterState, dataset_id: str) -> str:
    try:
        for dataset in await state.backend.list_datasets():
            if getattr(dataset, "id", None) == dataset_id:
                return str(getattr(dataset, "name", "") or dataset_id)
    except Exception:
        pass
    return dataset_id


async def _pending_count_for_dataset(state: DatasetRouterState, dataset_id: str) -> int:
    if not hasattr(state.backend, "health_snapshot"):
        return 0
    try:
        snapshot = await state.backend.health_snapshot()
    except Exception:
        return 0
    for dataset in snapshot.get("datasets", []):
        if dataset.get("id") == dataset_id:
            return int(dataset.get("pending_files") or 0)
    return 0


async def _parse_progress_snapshot(state: DatasetRouterState, dataset_id: str) -> dict[str, Any]:
    db = getattr(state.backend, "db", None)
    reader = getattr(db, "dataset_parse_progress", None)
    if not callable(reader):
        return {}
    try:
        return await asyncio.to_thread(reader, dataset_id)
    except Exception:
        return {}


async def _parse_with_job_progress(
    state: DatasetRouterState,
    *,
    dataset_id: str,
    dataset_name: str,
    limit: int,
    pending_before: int,
    processed_offset: int,
    job_id: str | None,
) -> Any:
    task = asyncio.create_task(state.backend.parse_dataset(dataset_id, limit=limit))
    if not job_id:
        return await task

    while True:
        done, _ = await asyncio.wait({task}, timeout=1.0)
        if done:
            return task.result()
        snapshot = await _parse_progress_snapshot(state, dataset_id)
        pending_now = int(snapshot.get("pending", pending_before) or 0)
        errors_now = int(snapshot.get("errors", 0) or 0)
        processed_batch = max(0, min(limit, pending_before - pending_now))
        processed = processed_offset + processed_batch
        file_name = str(snapshot.get("file_name") or "")
        stage = _PARSE_STAGE_LABELS.get(str(snapshot.get("stage") or ""), "обработка")
        current = Path(file_name).name if file_name else "подготовка следующего файла"
        message = f"{dataset_name}: {stage} · {current}"
        state.job_tracker[job_id].update(
            {
                "status": "PARSING",
                "processed": processed,
                "errors": errors_now,
                "message": message,
            }
        )
        state.job_service.update(
            job_id,
            status="running",
            processed=processed,
            errors=errors_now,
            message=message,
        )


def _parse_result_ready_for_reader(result: Any) -> bool:
    if not isinstance(result, dict):
        return False
    if result.get("status") != "completed":
        return False
    if int(result.get("errors") or 0):
        return False
    return int(result.get("remaining_pending") or 0) == 0


def _schedule_reader_after_parse(dataset_id: str, *, reason: str, parse_result: Any) -> dict[str, Any] | None:
    if not _parse_result_ready_for_reader(parse_result):
        return None
    result = schedule_dataset_reader_pass(dataset_id, reason=reason, force=True, require_enabled=True)
    return result if result.get("scheduled") else None


def _processed_from_parse_result(result: Any, *, pending_before: int, batch_limit: int, fallback_total: int) -> int:
    if isinstance(result, dict):
        parsed = result.get("files_parsed")
        if parsed is not None:
            try:
                return max(0, int(parsed))
            except (TypeError, ValueError):
                pass
        remaining = result.get("remaining_pending")
        if remaining is not None:
            try:
                return max(0, min(batch_limit, pending_before - int(remaining)))
            except (TypeError, ValueError):
                pass
    return max(0, int(fallback_total or 0))


async def run_dataset_parse_drain(
    state: DatasetRouterState,
    *,
    dataset_id: str,
    dataset_name: str,
    batch_limit: int,
    max_batches: int = DEFAULT_PARSE_DRAIN_MAX_BATCHES,
    job_id: str | None = None,
    reason: str = "dataset_parse_drain",
) -> dict[str, Any]:
    """Drain one dataset's PENDING queue in bounded batches.

    This is intentionally dataset-scoped, unlike the global parse scheduler. It is
    used after registering an external folder so Windows/Sovushka does not leave
    a newly-created dataset looking empty after the first 25-file batch.
    """
    batches: list[dict[str, Any]] = []
    parsed_batches = 0
    processed_files = 0
    errors = 0
    stop_reason = ""
    remaining_pending = await _pending_count_for_dataset(state, dataset_id)

    for batch_no in range(1, max(1, int(max_batches)) + 1):
        pending_before = await _pending_count_for_dataset(state, dataset_id)
        remaining_pending = pending_before
        if pending_before <= 0:
            break

        message = f"Ожидает очереди: {dataset_name} · осталось файлов {pending_before}"
        if job_id:
            state.job_tracker[job_id].update(
                {
                    "status": "QUEUED",
                    "processed": processed_files,
                    "total": max(processed_files + pending_before, processed_files + batch_limit),
                    "message": message,
                }
            )
            state.job_service.update(
                job_id,
                status="queued",
                processed=processed_files,
                total=max(processed_files + pending_before, processed_files + batch_limit),
                message=message,
            )

        await assert_parse_admission(state)
        async with state.parse_semaphore:
            if job_id:
                state.job_tracker[job_id].update(
                    {"status": "PARSING", "message": f"Начинаю обработку: {dataset_name}"}
                )
                state.job_service.update(
                    job_id, status="running", message=f"Начинаю обработку: {dataset_name}"
                )
            result = await _parse_with_job_progress(
                state,
                dataset_id=dataset_id,
                dataset_name=dataset_name,
                limit=batch_limit,
                pending_before=pending_before,
                processed_offset=processed_files,
                job_id=job_id,
            )

        parsed_batches += 1
        batch_errors = int(result.get("errors") or 0) if isinstance(result, dict) else 1
        if not isinstance(result, dict) or result.get("status") != "completed":
            batch_errors += 1
        errors += batch_errors
        batch_processed = _processed_from_parse_result(
            result,
            pending_before=pending_before,
            batch_limit=batch_limit,
            fallback_total=min(batch_limit, pending_before),
        )
        processed_files += batch_processed
        remaining_pending = int(result.get("remaining_pending") or 0) if isinstance(result, dict) else pending_before
        batches.append(
            {
                "batch": batch_no,
                "dataset_id": dataset_id,
                "dataset_name": dataset_name,
                "pending_before": pending_before,
                "processed": batch_processed,
                "limit": batch_limit,
                "result": result,
            }
        )
        reader_job = _schedule_reader_after_parse(dataset_id, reason=reason, parse_result=result)
        if reader_job:
            batches[-1]["dataset_reader"] = reader_job
        if batch_errors:
            stop_reason = "batch errors"
            break

    if remaining_pending > 0 and parsed_batches >= max_batches:
        stop_reason = stop_reason or f"max_batches={max_batches} reached"

    status = "completed" if remaining_pending == 0 and errors == 0 else "partial" if parsed_batches else "idle"
    if errors:
        status = "partial"
    result = {
        "status": status,
        "dataset_id": dataset_id,
        "dataset_name": dataset_name,
        "batch_limit": batch_limit,
        "max_batches": max_batches,
        "batches": batches,
        "batches_run": parsed_batches,
        "processed_files": processed_files,
        "errors": errors,
        "remaining_pending": remaining_pending,
        "stop_reason": stop_reason,
    }
    if job_id:
        tracker_status = "COMPLETED" if status == "completed" else "PARTIAL" if status == "partial" else "IDLE"
        service_status = "failed" if errors else "completed"
        state.job_tracker[job_id].update(
            {
                "status": tracker_status,
                "processed": processed_files,
                "errors": errors,
                "finished_at": datetime.now().isoformat(),
                "message": (
                    f"Готово: {dataset_name} · batches={parsed_batches} · "
                    f"файлов={processed_files} · pending={remaining_pending} · errors={errors}"
                ),
                "result": result,
            }
        )
        state.job_service.update(
            job_id,
            status=service_status,
            processed=processed_files,
            errors=errors,
            message=state.job_tracker[job_id]["message"],
            result=result,
        )
    return result


async def run_parse_scheduler(
    state: DatasetRouterState,
    req: ParseSchedulerRequest,
    job_id: str | None = None,
) -> dict[str, Any]:
    batches = []
    parsed_batches = 0
    errors = 0
    remaining_pending = 0
    stop_reason = ""
    final_unload = None
    min_free_gb = req.min_free_gb if req.min_free_gb is not None else PARSE_MIN_FREE_GB
    max_swap_pct = req.max_swap_pct if req.max_swap_pct is not None else PARSE_MAX_SWAP_PCT
    post_batch_min_free_gb = req.post_batch_min_free_gb if req.post_batch_min_free_gb is not None else min_free_gb
    post_batch_max_swap_pct = (
        req.post_batch_max_swap_pct if req.post_batch_max_swap_pct is not None else PARSE_POST_MAX_SWAP_PCT
    )
    priority_order = active_parse_priority_order(state.current_mode, req.dataset_priority_order)


    for batch_no in range(1, req.max_batches + 1):
        queue = await pending_parse_datasets(state, priority_order)
        remaining_pending = sum(item["pending_files"] for item in queue)
        if not queue:
            break

        target = queue[0]
        message = (
            f"Batch {batch_no}/{req.max_batches}: {target['dataset_name']} "
            f"pending={target['pending_files']}"
        )
        if job_id:
            state.job_tracker[job_id].update(
                {
                    "status": "PARSING",
                    "processed": parsed_batches,
                    "total": req.max_batches,
                    "message": message,
                }
            )
            state.job_service.update(job_id, processed=parsed_batches, total=req.max_batches, message=message)

        await assert_parse_admission(state, min_free_gb=min_free_gb, max_swap_pct=max_swap_pct)
        # The durable auto-resume scheduler shares the same parse owner as upload,
        # dataset-drain and manual batch jobs. Without this guard, a document uploaded
        # during the supervisor's six-second startup window can be parsed twice and
        # leave two Qdrant points for one MetaDB chunk.
        async with state.parse_semaphore:
            result = await state.backend.parse_dataset(target["dataset_id"], limit=req.batch_limit)
        parsed_batches += 1
        batch_errors = int(result.get("errors") or 0) if isinstance(result, dict) else 1
        if not isinstance(result, dict) or result.get("status") not in {"completed"}:
            batch_errors += 1
        errors += batch_errors
        batches.append(
            {
                "batch": batch_no,
                "dataset_id": target["dataset_id"],
                "dataset_name": target["dataset_name"],
                "pending_before": target["pending_files"],
                "limit": req.batch_limit,
                "result": result,
            }
        )
        reader_job = _schedule_reader_after_parse(
            target["dataset_id"],
            reason="parse_scheduler_batch",
            parse_result=result,
        )
        if reader_job:
            batches[-1]["dataset_reader"] = reader_job


        try:
            post_memory = await parse_memory_state()
            batches[-1]["post_memory"] = post_memory
            if post_memory["ram_free_gb"] < post_batch_min_free_gb:
                stop_reason = (
                    f"post-batch memory guard: ram_free_gb={post_memory['ram_free_gb']} "
                    f"< {post_batch_min_free_gb}"
                )
                break
            if post_memory["swap_pct"] > post_batch_max_swap_pct:
                stop_reason = (
                    f"post-batch memory guard: swap_pct={post_memory['swap_pct']} "
                    f"> {post_batch_max_swap_pct}"
                )
                break
        except HTTPException as memory_error:
            stop_reason = f"post-batch memory check failed: {memory_error.detail}"
            break

        if batch_errors and req.stop_on_error:
            break
        if batch_no < req.max_batches and req.cooldown_sec > 0:
            await asyncio.sleep(req.cooldown_sec)


    queue = await pending_parse_datasets(state, priority_order)
    remaining_pending = sum(item["pending_files"] for item in queue)
    status = "completed" if remaining_pending == 0 and errors == 0 else "partial" if batches else "idle"
    if errors:
        status = "partial"

    result = {
        "status": status,
        "runtime_profile": current_runtime_profile(state.current_mode),
        "batch_limit": req.batch_limit,
        "max_batches": req.max_batches,
        "dataset_priority_order": priority_order,
        "batches": batches,
        "batches_run": parsed_batches,
        "errors": errors,
        "remaining_pending": remaining_pending,
        "datasets_pending": queue,
        "stop_reason": stop_reason,
        "final_unload": final_unload,
    }

    if job_id:
        service_status = "completed" if status in {"completed", "idle"} else "failed" if errors else "completed"
        state.job_tracker[job_id].update(
            {
                "status": status.upper(),
                "processed": parsed_batches,
                "finished_at": datetime.now().isoformat(),
                "message": (
                    f"Готово: batches={parsed_batches}, pending={remaining_pending}, errors={errors}"
                ),
                "result": result,
            }
        )
        state.job_service.update(
            job_id,
            status=service_status,
            processed=parsed_batches,
            errors=errors,
            message=state.job_tracker[job_id]["message"],
            result=result,
        )
    return result
