"""Dataset uploads endpoints."""
from __future__ import annotations
from proxy.services import chat_attachment_read_service as attachment_reader
import asyncio
import logging
import os
from dataclasses import asdict
from pathlib import Path
from typing import Annotated, Any
from fastapi import APIRouter, Depends, File, Form, Header, HTTPException, Query, UploadFile
from backend.document_router import route_document
from proxy.config import max_upload_bytes, rag_upload_suffixes
from proxy.security import require_admin, require_user
from proxy.services.candidate_acceptance_service import (
    CandidateAcceptanceError,
    require_candidate_acceptance,
)
from proxy.storage.file_storage import safe_upload_name, save_upload_tmp, validate_external_source

from proxy.services.dataset_contracts import (ChatFolderRead, _CHAT_ATTACH_DATASET_NAME)
from proxy.services.dataset_runtime import DatasetRouterState

import proxy.services.dataset_parse_service as dataset_scheduler
import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])
search_router = APIRouter(prefix="/api", tags=["attachments"])


async def _dataset_id_for_name(state: DatasetRouterState, dataset_name: str) -> tuple[str, bool]:
    ds_list = await state.backend.list_datasets()
    ds = next((dataset for dataset in ds_list if dataset.name == dataset_name), None)
    if ds:
        return ds.id, False
    return await state.backend.create_dataset(dataset_name), True


def _upload_intake_response(temp_path: Path, original_name: str) -> dict[str, Any]:
    size = temp_path.stat().st_size
    if size <= 0:
        raise HTTPException(status_code=400, detail="Файл пустой")
    return {
        "accepted": True,
        "reason": "accepted",
        "file_name": original_name,
        "suffix": temp_path.suffix.lower(),
        "size_bytes": size,
    }


async def _record_background_parse_error(
    state: DatasetRouterState,
    *,
    dataset_id: str,
    document_id: str,
    error: Exception,
) -> None:
    """Make an asynchronous intake failure visible to API/UI operators."""
    diagnostic = (
        f"BACKGROUND_PARSE_FAILED [{type(error).__name__}]: "
        f"{str(error) or 'exception without message'}"
    )
    marker = getattr(state.backend, "mark_document_error", None)
    if not callable(marker):
        logger.error(
            "[UPLOAD PARSE] dataset=%s document=%s failed and backend cannot persist the error: %s",
            dataset_id,
            document_id,
            error,
            exc_info=True,
        )
        return
    try:
        await marker(dataset_id, document_id, diagnostic)
    except Exception as marker_error:  # noqa: BLE001 - retain the original failure in logs
        logger.error(
            "[UPLOAD PARSE] failed to persist dataset=%s document=%s error: %s",
            dataset_id,
            document_id,
            marker_error,
            exc_info=True,
        )
    logger.error(
        "[UPLOAD PARSE] dataset=%s document=%s failed: %s",
        dataset_id,
        document_id,
        error,
        exc_info=True,
    )


@router.post("/upload/{dataset_id}")
async def upload_file(dataset_id: str, file: UploadFile = File(...), _admin=Depends(require_admin)):
    state = dataset_runtime.get_dataset_state()
    original_name = safe_upload_name(file.filename or "upload.bin", rag_upload_suffixes())
    temp_path = await save_upload_tmp(
        file,
        allowed_suffixes=rag_upload_suffixes(),
        max_bytes=max_upload_bytes(),
    )
    doc_id = await state.backend.upload_file(dataset_id, temp_path, relative_path=original_name)

    async def _parse():
        # Дренаж очереди: один upload разгребает батч PENDING (а не 1 док — иначе
        # пачка аплоадов оставляет хвост висеть, см. фикс индексатора 2026-06-17).
        try:
            limit = int(os.getenv("LES_UPLOAD_PARSE_LIMIT", "25"))
        except ValueError:
            limit = 25
        try:
            async with state.parse_semaphore:
                await dataset_scheduler.assert_parse_admission(state)
                await state.backend.parse_dataset(dataset_id, limit=limit)
        except Exception as error:  # noqa: BLE001 - background task must reach a terminal state
            await _record_background_parse_error(
                state,
                dataset_id=dataset_id,
                document_id=doc_id,
                error=error,
            )
        finally:
            temp_path.unlink(missing_ok=True)

    asyncio.create_task(_parse())
    return {"doc_id": doc_id, "status": "queued"}


@search_router.post("/chat/attachments", status_code=201)
async def create_chat_attachment(
    file: UploadFile = File(...),
    candidate_acceptance: Annotated[bool, Form()] = False,
    idempotency_key: Annotated[str, Header(alias="Idempotency-Key")] = "",
    _user=Depends(require_user),
):
    """Official external intake: one temporary read attachment for the next chat turn."""
    try:
        require_candidate_acceptance(
            requested=candidate_acceptance,
            user=_user,
        )
    except CandidateAcceptanceError as error:
        status_code = 403 if "ROOT_ADMIN" in str(error) else 409
        raise HTTPException(status_code, str(error)) from error
    from proxy.services.request_idempotency_service import (
        IdempotencyConflict,
        begin,
        caller_scope,
        complete,
        file_sha256,
        release,
        request_fingerprint,
    )

    from backend.product_edition import is_light
    from proxy.services.chat_attachment_service import IMAGE_SUFFIXES
    suffixes = rag_upload_suffixes() | (IMAGE_SUFFIXES if is_light() else set())
    original_name = safe_upload_name(file.filename or "upload.bin", suffixes)
    temp_path = await save_upload_tmp(
        file,
        allowed_suffixes=suffixes,
        max_bytes=max_upload_bytes(),
    )
    caller = caller_scope(_user)
    fingerprint = request_fingerprint(
        {
            "name": original_name,
            "size": temp_path.stat().st_size,
            "sha256": await asyncio.to_thread(file_sha256, temp_path),
        }
    )
    try:
        try:
            state, cached = await asyncio.to_thread(
                begin,
                operation="chat_attachment",
                caller=caller,
                idempotency_key=idempotency_key,
                request_hash=fingerprint,
            )
        except (ValueError, IdempotencyConflict) as error:
            raise HTTPException(409, str(error)) from error
        if state == "completed" and cached is not None:
            return cached
        if state == "in_progress":
            raise HTTPException(
                409,
                "Вложение с этим Idempotency-Key уже обрабатывается",
                headers={"Retry-After": "2"},
            )
        try:
            result = await attachment_reader._prepare_read_attachment(temp_path, original_name)
            await asyncio.to_thread(
                complete,
                operation="chat_attachment",
                caller=caller,
                idempotency_key=idempotency_key,
                request_hash=fingerprint,
                response=result,
            )
            return result
        except Exception:
            await asyncio.to_thread(
                release,
                operation="chat_attachment",
                caller=caller,
                idempotency_key=idempotency_key,
                request_hash=fingerprint,
            )
            raise
    finally:
        temp_path.unlink(missing_ok=True)


async def _ensure_chat_attach_dataset(state) -> str:
    for d in await state.backend.list_datasets():
        if d.name == _CHAT_ATTACH_DATASET_NAME:
            return d.id
    return await state.backend.create_dataset(_CHAT_ATTACH_DATASET_NAME)


@router.post("/attach")
async def attach_chat_file(
    file: UploadFile = File(...),
    mode: str = Query("read", pattern="^(read|quick|index)$"),
    _admin=Depends(require_admin),
):
    """Скрепка чата.

    ``mode=read`` — прочитать файл в markdown и вернуть текст как контекст следующего запроса
    (без индексации); ``mode=quick`` — быстрый парс таблиц в Parquet (без векторов), сразу
    доступно сверке/таблицам; ``mode=index`` — полная индексация в датасет «Вложения чата»
    (вектора в Qdrant). На парсе таблиц LLM не участвует (ADR-11).
    """
    from uuid import uuid4

    state = dataset_runtime.get_dataset_state()
    from backend.product_edition import is_light
    from proxy.services.chat_attachment_service import IMAGE_SUFFIXES
    suffixes = rag_upload_suffixes() | (IMAGE_SUFFIXES if is_light() else set())
    original_name = safe_upload_name(file.filename or "upload.bin", suffixes)
    temp_path = await save_upload_tmp(file, allowed_suffixes=suffixes, max_bytes=max_upload_bytes())

    if mode not in {"read", "quick", "index"}:
        temp_path.unlink(missing_ok=True)
        raise HTTPException(422, "Неизвестный режим вложения")
    # Old clients may still request quick mode. Light uses the same preserved
    # read attachment for every document instead of the legacy table harness.
    if mode in {"read", "quick"}:
        try:
            return await attachment_reader._prepare_read_attachment(temp_path, original_name)
        finally:
            temp_path.unlink(missing_ok=True)

    if mode == "index":
        ds_id = await _ensure_chat_attach_dataset(state)
        doc_id = await state.backend.upload_file(ds_id, temp_path, relative_path=original_name)

        async def _parse():
            try:
                async with state.parse_semaphore:
                    await dataset_scheduler.assert_parse_admission(state)
                    await state.backend.parse_dataset(ds_id, limit=25)
            except Exception as error:  # noqa: BLE001 - background task must reach a terminal state
                await _record_background_parse_error(
                    state,
                    dataset_id=ds_id,
                    document_id=doc_id,
                    error=error,
                )
            finally:
                temp_path.unlink(missing_ok=True)

        asyncio.create_task(_parse())
        return {"attachment_id": ds_id, "mode": "index", "name": original_name,
                "dataset_name": _CHAT_ATTACH_DATASET_NAME, "doc_id": doc_id, "status": "queued"}


@router.post('/attach-folder')
async def attach_chat_folder(req: ChatFolderRead, _admin=Depends(require_admin)):
    from proxy.services.chat_folder_service import read_folder
    root = validate_external_source(req.path)
    state = dataset_runtime.get_dataset_state()
    async with state.parse_semaphore:
        await dataset_scheduler.assert_parse_admission(state)
        try:
            return await asyncio.to_thread(read_folder, root)
        except (ValueError, OSError) as error:
            raise HTTPException(422, str(error)) from error


@router.post("/upload-smart")
async def upload_file_smart(
    file: UploadFile = File(...),
    parse: bool = Query(default=True),
    _admin=Depends(require_admin),
):
    state = dataset_runtime.get_dataset_state()
    original_name = safe_upload_name(file.filename or "upload.bin", rag_upload_suffixes())
    temp_path = await save_upload_tmp(
        file,
        allowed_suffixes=rag_upload_suffixes(),
        max_bytes=max_upload_bytes(),
    )
    try:
        intake = _upload_intake_response(temp_path, original_name)
        route = await asyncio.to_thread(route_document, temp_path)
        dataset_id, created = await _dataset_id_for_name(state, route.dataset_name)
        doc_id = await state.backend.upload_file(dataset_id, temp_path, relative_path=original_name)

        if parse:
            async def _parse():
                try:
                    async with state.parse_semaphore:
                        await dataset_scheduler.assert_parse_admission(state)
                        await state.backend.parse_dataset(dataset_id, limit=1)
                except Exception as error:  # noqa: BLE001 - background task must reach a terminal state
                    await _record_background_parse_error(
                        state,
                        dataset_id=dataset_id,
                        document_id=doc_id,
                        error=error,
                    )
                finally:
                    temp_path.unlink(missing_ok=True)

            asyncio.create_task(_parse())
            status = "queued"
        else:
            temp_path.unlink(missing_ok=True)
            status = "registered"

        return {
            "doc_id": doc_id,
            "status": status,
            "dataset_id": dataset_id,
            "dataset_name": route.dataset_name,
            "dataset_created": created,
            "intake": intake,
            "route": asdict(route),
            "parse_started": parse,
        }
    except Exception:
        temp_path.unlink(missing_ok=True)
        raise
