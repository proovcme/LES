"""Dataset parse endpoints."""
from __future__ import annotations
import asyncio
import logging
import os
import sqlite3
from datetime import datetime
from pathlib import Path
from fastapi import APIRouter, Depends, HTTPException, Query
from backend.interface import DatasetInfo
from backend.rag_config import rag_meta_db_path
from backend.smart_index import SKIP_DIRS, verify_source_file
from proxy.security import require_admin
from proxy.storage.file_storage import validate_source_folder

from proxy.services.dataset_contracts import (DEFAULT_PARSE_BATCH_LIMIT, PARSE_MAX_SWAP_PCT, PARSE_MIN_FREE_GB, ParseSchedulerRequest, UUID_RE)

import proxy.services.dataset_parse_service as dataset_scheduler
import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])


@router.post("/sync/{folder}")
async def sync_folder(folder: str, _admin=Depends(require_admin)):
    state = dataset_runtime.get_dataset_state()
    src_dir = validate_source_folder(folder)
    if UUID_RE.match(folder) or folder in SKIP_DIRS:
        raise HTTPException(status_code=400, detail=f"source folder is excluded from RAG sync: {folder}")
    source_root = Path("./RAG_Content")
    source_paths = list(src_dir.rglob("*"))
    decisions = [verify_source_file(path, source_root) for path in source_paths]
    files = [path for path, decision in zip(source_paths, decisions) if decision.accepted]
    if not files:
        raise HTTPException(status_code=400, detail=f"no supported documents found in source folder: {folder}")
    rejected_reasons: dict[str, int] = {}
    for decision in decisions:
        if decision.reason in {"accepted", "not_file"}:
            continue
        rejected_reasons[decision.reason] = rejected_reasons.get(decision.reason, 0) + 1

    ds_list = await state.backend.list_datasets()
    ds_name = f"{folder}_Index"
    ds = next((dataset for dataset in ds_list if dataset.name == ds_name), None)
    if not ds:
        ds_id = await state.backend.create_dataset(ds_name)
        ds = DatasetInfo(id=ds_id, name=ds_name, status="IDLE", doc_count=0, chunk_count=0)

    now_ts = datetime.now()
    stale = [
        key
        for key, value in state.job_tracker.items()
        if value.get("started_at")
        and (now_ts - datetime.fromisoformat(value["started_at"])).total_seconds() > 86400
    ]
    for key in stale:
        del state.job_tracker[key]

    job = state.job_service.create(
        "rag_sync",
        source=folder,
        dataset_id=ds.id,
        dataset_name=ds_name,
        status="running",
        message="Сканирование...",
    )
    job_id = job["id"]
    state.job_tracker[job_id] = {
        "dataset_id": ds.id,
        "dataset_name": ds_name,
        "status": "SCANNING",
        "total": 0,
        "processed": 0,
        "started_at": job["started_at"],
        "message": "Сканирование...",
    }

    dest_dir = Path(f"./storage/datasets/{ds.id}")
    dest_dir.mkdir(parents=True, exist_ok=True)
    new_count, skip_count, changed_count = 0, 0, 0
    state.job_tracker[job_id]["total"] = len(files)
    state.job_service.update(job_id, total=len(files), status="running")

    for index, source_file in enumerate(files):
        rel_path = source_file.relative_to(src_dir)
        dest = dest_dir / rel_path
        dest.parent.mkdir(parents=True, exist_ok=True)

        stat = source_file.stat()
        is_new = not dest.exists()
        is_changed = False
        if not is_new:
            dest_stat = dest.stat()
            if stat.st_size != dest_stat.st_size or abs(stat.st_mtime - dest_stat.st_mtime) > 1.0:
                is_changed = True
        if is_new or is_changed:
            await state.backend.upload_file(ds.id, source_file, relative_path=rel_path.as_posix())
            if is_new:
                new_count += 1
            else:
                changed_count += 1
        else:
            skip_count += 1
        state.job_tracker[job_id]["processed"] = index + 1
        state.job_tracker[job_id]["message"] = (
            f"{'Новый' if is_new else 'Обновлён' if is_changed else 'Пропущен'}: {source_file.name}"
        )
        state.job_service.update(job_id, processed=index + 1, message=state.job_tracker[job_id]["message"])
        if (index + 1) % 20 == 0 or is_new or is_changed:
            state.log_history.append(
                f"[JOB {job_id}] {source_file.name} ({index + 1}/{len(files)}): "
                f"{'NEW' if is_new else 'CHANGED' if is_changed else 'SKIP'}"
            )
        await asyncio.sleep(0.1)

    force_reindex = (new_count + changed_count) == 0 and (ds.chunk_count or 0) == 0 and skip_count > 0

    if force_reindex:
        state.job_tracker[job_id]["status"] = "PARSING"
        state.job_tracker[job_id]["message"] = f"Индекс пуст — принудительная переиндексация {skip_count} файлов"
        state.job_service.update(job_id, status="running", message=state.job_tracker[job_id]["message"])
        logger.info("[JOB %s] Force reindex: 0 chunks in Qdrant, %s files on disk", job_id, skip_count)
    else:
        has_changes = (new_count + changed_count) > 0
        state.job_tracker[job_id]["status"] = "PARSING" if has_changes else "COMPLETED"
        state.job_tracker[job_id]["message"] = (
            f"Векторизация bge-m3: {new_count} новых, {changed_count} изм."
            if has_changes
            else f"Нет изменений (пропущено {skip_count})"
        )
        state.job_service.update(
            job_id,
            status="running" if has_changes else "completed",
            message=state.job_tracker[job_id]["message"],
            result={"new_files": new_count, "changed_files": changed_count, "skipped_files": skip_count},
        )

    if (new_count + changed_count) > 0 or force_reindex:
        async def _run():
            try:
                async with state.sync_parse_semaphore:
                    os.nice(10)
                    await dataset_scheduler.assert_parse_admission(state)
                    result = await state.backend.parse_dataset(ds.id, limit=DEFAULT_PARSE_BATCH_LIMIT)
                chunks = result.get("chunks", 0) if isinstance(result, dict) else 0
                elapsed = result.get("elapsed_sec", 0) if isinstance(result, dict) else 0
                errors = result.get("errors", 0) if isinstance(result, dict) else 0
                remaining = result.get("remaining_pending", 0) if isinstance(result, dict) else 0
                result_status = result.get("status", "unknown") if isinstance(result, dict) else "unknown"
                if result_status != "completed" or errors:
                    final_status = "FAILED"
                    service_status = "failed"
                elif remaining:
                    final_status = "PARTIAL"
                    service_status = "completed"
                else:
                    final_status = "COMPLETED"
                    service_status = "completed"
                state.job_tracker[job_id]["status"] = final_status
                state.job_tracker[job_id]["finished_at"] = datetime.now().isoformat()
                state.job_tracker[job_id]["message"] = (
                    f"Готово: +{new_count} новых, ~{changed_count} обновлённых, "
                    f"пропущено {skip_count} | {chunks} чанков | {elapsed:.0f}с | "
                    f"осталось pending={remaining}, errors={errors}"
                )
                state.job_service.update(
                    job_id,
                    status=service_status,
                    message=state.job_tracker[job_id]["message"],
                    result={
                        "new_files": new_count,
                        "changed_files": changed_count,
                        "skipped_files": skip_count,
                        "chunks": chunks,
                        "elapsed_sec": elapsed,
                        "remaining_pending": remaining,
                        "errors": errors,
                        "parse_status": result_status,
                    },
                )
                reader_job = dataset_scheduler._schedule_reader_after_parse(ds.id, reason="sync_folder_parse", parse_result=result)
                if reader_job:
                    state.job_tracker[job_id]["dataset_reader"] = reader_job
                logger.info(
                    "[JOB %s] %s: %s chunks, %.0fs, remaining=%s, errors=%s",
                    job_id, final_status, chunks, elapsed, remaining, errors,
                )
            except Exception as e:
                state.job_tracker[job_id]["status"] = "FAILED"
                state.job_tracker[job_id]["finished_at"] = datetime.now().isoformat()
                state.job_tracker[job_id]["message"] = f"Ошибка: {str(e)}"
                state.job_service.update(job_id, status="failed", errors=1, message=state.job_tracker[job_id]["message"])
                logger.error("[JOB %s] FAILED: %s", job_id, e, exc_info=True)

        asyncio.create_task(_run())

    return {
        "status": "sync_started",
        "job_id": job_id,
        "dataset_id": ds.id,
        "new_files": new_count,
        "changed_files": changed_count,
        "skipped_files": skip_count,
        "rejected_files": sum(rejected_reasons.values()),
        "rejected_reasons": rejected_reasons,
    }


@router.post("/parse-batch/{dataset_id}")
async def parse_dataset_batch(
    dataset_id: str,
    limit: int = Query(default=DEFAULT_PARSE_BATCH_LIMIT, ge=1, le=25),
    background: bool = False,
    _admin=Depends(require_admin),
):
    state = dataset_runtime.get_dataset_state()
    if background:
        dataset_name = await dataset_scheduler._dataset_name_for_id(state, dataset_id)
        pending = await dataset_scheduler._pending_count_for_dataset(state, dataset_id)
        total = min(limit, pending) if pending > 0 else limit
        job = state.job_service.create(
            "rag_parse_batch",
            source="dataset",
            dataset_id=dataset_id,
            dataset_name=dataset_name,
            status="queued",
            total=total,
            message=f"Ожидает очереди: {dataset_name} · до {limit} файлов",
        )
        job_id = job["id"]
        state.job_tracker[job_id] = {
            "id": job_id,
            "type": "rag_parse_batch",
            "status": "QUEUED",
            "source": "dataset",
            "dataset_id": dataset_id,
            "dataset_name": dataset_name,
            "total": total,
            "processed": 0,
            "errors": 0,
            "started_at": job.get("started_at"),
            "message": f"Ожидает очереди: {dataset_name} · до {limit} файлов",
        }

        async def _run():
            try:
                async with state.parse_semaphore:
                    await dataset_scheduler.assert_parse_admission(state)
                    state.job_tracker[job_id].update(
                        {"status": "PARSING", "message": f"Начинаю обработку: {dataset_name}"}
                    )
                    state.job_service.update(
                        job_id, status="running", message=f"Начинаю обработку: {dataset_name}"
                    )
                    result = await dataset_scheduler._parse_with_job_progress(
                        state,
                        dataset_id=dataset_id,
                        dataset_name=dataset_name,
                        limit=limit,
                        pending_before=pending,
                        processed_offset=0,
                        job_id=job_id,
                    )
                chunks = int(result.get("chunks") or 0) if isinstance(result, dict) else 0
                errors = int(result.get("errors") or 0) if isinstance(result, dict) else 1
                remaining = int(result.get("remaining_pending") or 0) if isinstance(result, dict) else 0
                status = str(result.get("status") or "unknown") if isinstance(result, dict) else "unknown"
                processed = dataset_scheduler._processed_from_parse_result(
                    result,
                    pending_before=pending,
                    batch_limit=limit,
                    fallback_total=total,
                )
                if status == "completed" and not errors and remaining <= 0:
                    service_status = "completed"
                    tracker_status = "COMPLETED"
                elif status == "completed" and not errors:
                    service_status = "completed"
                    tracker_status = "PARTIAL"
                else:
                    service_status = "failed"
                    tracker_status = "FAILED"
                    errors = max(errors, 1)
                message = (
                    f"Партия готова: {dataset_name} · файлов {processed}/{total} · "
                    f"+{chunks} чанков · ошибок {errors} · осталось pending={remaining}"
                )
                state.job_tracker[job_id].update(
                    {
                        "status": tracker_status,
                        "processed": processed,
                        "errors": errors,
                        "finished_at": datetime.now().isoformat(),
                        "message": message,
                        "result": result if isinstance(result, dict) else {"status": status},
                    }
                )
                state.job_service.update(
                    job_id,
                    status=service_status,
                    processed=processed,
                    errors=errors,
                    message=message,
                    result=result if isinstance(result, dict) else {"status": status},
                )
                reader_job = dataset_scheduler._schedule_reader_after_parse(dataset_id, reason="parse_batch_background", parse_result=result)
                if reader_job:
                    state.job_tracker[job_id]["dataset_reader"] = reader_job
            except Exception as error:
                detail = getattr(error, "detail", None) or str(error)
                message = f"Ошибка парсинга: {detail}"
                state.job_tracker[job_id].update(
                    {
                        "status": "FAILED",
                        "errors": 1,
                        "finished_at": datetime.now().isoformat(),
                        "message": message,
                    }
                )
                state.job_service.update(job_id, status="failed", errors=1, message=message)
                logger.error("[PARSE_BATCH %s] FAILED: %s", job_id, detail, exc_info=True)

        asyncio.create_task(_run())
        return {
            "status": "queued",
            "job_id": job_id,
            "dataset_id": dataset_id,
            "dataset_name": dataset_name,
            "limit": limit,
            "pending": pending,
        }

    async with state.parse_semaphore:
        await dataset_scheduler.assert_parse_admission(state)
        result = await state.backend.parse_dataset(dataset_id, limit=limit)
    response = {"dataset_id": dataset_id, "limit": limit, "result": result}
    reader_job = dataset_scheduler._schedule_reader_after_parse(dataset_id, reason="parse_batch", parse_result=result)
    if reader_job:
        response["dataset_reader"] = reader_job
    return response


@router.post("/parse-scheduler")
async def parse_scheduler(req: ParseSchedulerRequest, _admin=Depends(require_admin)):
    state = dataset_runtime.get_dataset_state()
    active_job = dataset_scheduler.active_parse_scheduler_job(state)
    if active_job:
        job_id, job = active_job
        raise HTTPException(
            status_code=409,
            detail=(
                f"Parse scheduler already active: {job_id} "
                f"{job.get('status', '')} {job.get('message', '')}"
            ),
        )

    min_free_gb = req.min_free_gb if req.min_free_gb is not None else PARSE_MIN_FREE_GB
    max_swap_pct = req.max_swap_pct if req.max_swap_pct is not None else PARSE_MAX_SWAP_PCT
    await dataset_scheduler.assert_parse_admission(state, min_free_gb=min_free_gb, max_swap_pct=max_swap_pct)

    if not req.background:
        return await dataset_scheduler.run_parse_scheduler(state, req)

    try:
        conn = sqlite3.connect(rag_meta_db_path())
        pending_count = int(conn.execute("SELECT COUNT(*) FROM documents WHERE upper(status) = 'PENDING'").fetchone()[0] or 0)
        conn.close()
    except Exception:
        pending_count = req.max_batches
    effective_total = min(req.max_batches, pending_count) if pending_count > 0 else req.max_batches

    job = state.job_service.create(
        "rag_parse_scheduler",
        source="pending",
        status="running",
        message="Parse scheduler queued",
        total=effective_total,
    )
    job_id = job["id"]
    state.job_tracker[job_id] = {
        "type": "rag_parse_scheduler",
        "status": "QUEUED",
        "total": effective_total,
        "processed": 0,
        "started_at": job["started_at"],
        "message": "Parse scheduler queued",
    }

    async def _run():
        try:
            await dataset_scheduler.run_parse_scheduler(state, req, job_id=job_id)
        except Exception as error:
            state.job_tracker[job_id]["status"] = "FAILED"
            state.job_tracker[job_id]["finished_at"] = datetime.now().isoformat()
            state.job_tracker[job_id]["message"] = f"Ошибка scheduler: {error}"
            state.job_service.update(job_id, status="failed", errors=1, message=state.job_tracker[job_id]["message"])
            logger.error("[PARSE_SCHEDULER %s] FAILED: %s", job_id, error, exc_info=True)

    asyncio.create_task(_run())
    return {
        "status": "queued",
        "job_id": job_id,
        "batch_limit": req.batch_limit,
        "max_batches": req.max_batches,
    }
