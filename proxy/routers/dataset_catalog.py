"""Dataset catalog endpoints."""
from __future__ import annotations
import asyncio
import logging
import os
import sqlite3
from pathlib import Path
from backend.runtime_paths import mutable_path
from typing import Annotated, Any
from fastapi import APIRouter, Depends, Header, HTTPException, Query
from backend.rag_config import rag_collection_name, rag_meta_db_path
from backend.smart_index import SKIP_DIRS, should_index_source_file
from proxy.security import require_admin, require_root_admin, require_user
from proxy.services.context_memory_service import (
    benchmark_dataset_profile_warmup,
    build_dataset_profile,
    get_dataset_profile,
    set_dataset_kind,
    set_dataset_operator_guidance,
    warmup_dataset_profiles,
)
from proxy.services.dataset_memory_service import latest_file_cards
from proxy.services.rag_readiness_service import rag_readiness
from proxy.storage.file_storage import validate_external_source

from proxy.services.dataset_contracts import (CreateDatasetRequest, DatasetGroupPayload, DatasetGuidanceRequest, DatasetKindRequest, DatasetNamePayload, DatasetProfileWarmupRequest, DatasetWatchRequest, UUID_RE)

import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])


@router.get("/datasets/{dataset_id}/watch")
async def dataset_watch_status(dataset_id: str, _user=Depends(require_user)):
    from proxy.services.dataset_watch_service import watcher
    return await asyncio.to_thread(watcher().status, dataset_id)


@router.put("/datasets/{dataset_id}/watch")
async def configure_dataset_watch(dataset_id: str, req: DatasetWatchRequest, _admin=Depends(require_admin)):
    from proxy.services.dataset_watch_service import watcher
    root = validate_external_source(req.path) if req.enabled else req.path
    rows = await dataset_runtime.get_dataset_state().backend.list_datasets()
    if not any(row.id == dataset_id for row in rows):
        raise HTTPException(404, "Датасет не найден. Обновите список.")
    return await asyncio.to_thread(watcher().configure, dataset_id, root, enabled=req.enabled, auto_index=req.auto_index)


@router.get("/readiness")
async def get_rag_readiness(
    dataset_id: str | None = Query(default=None, max_length=160),
    force: bool = Query(default=False),
    _user=Depends(require_user),
):
    """Operator-visible dense/sparse/RRF and contract readiness."""
    return await asyncio.to_thread(rag_readiness, dataset_id=dataset_id, force=force)


@router.get("/catalog-consistency")
async def get_catalog_consistency(_user=Depends(require_user)):
    from proxy.services.rag_catalog_guard_service import catalog_guard_state

    return catalog_guard_state()


@router.post("/catalog-consistency/repair")
async def repair_catalog_consistency(_admin=Depends(require_root_admin)):
    from proxy.services.rag_catalog_guard_service import run_catalog_guard

    result = await run_catalog_guard(
        qdrant_url=os.getenv("QDRANT_URL", "http://127.0.0.1:6333"),
        collection=rag_collection_name(),
        meta_db_path=rag_meta_db_path(),
        apply=True,
    )
    if result.get("status") == "blocked":
        raise HTTPException(status_code=503, detail=result)
    return result


@router.get("/catalog-consistency/navigation-counts")
async def audit_navigation_count_consistency(_user=Depends(require_user)):
    """Read-only audit of legacy hierarchy counters; visible to the operator."""
    backend = dataset_runtime.get_dataset_state().backend
    return await asyncio.to_thread(
        backend.reconcile_legacy_navigation_counts,
        apply=False,
    )


@router.post("/catalog-consistency/navigation-counts/repair")
async def repair_navigation_count_consistency(_admin=Depends(require_root_admin)):
    """Apply only exact navigation-only metadata deltas; never delete or reindex."""
    backend = dataset_runtime.get_dataset_state().backend
    return await asyncio.to_thread(
        backend.reconcile_legacy_navigation_counts,
        apply=True,
    )


@router.delete("/datasets/{dataset_id}")
async def delete_dataset(
    dataset_id: str,
    recovery_policy: str = Query(
        default="required",
        pattern="^(required|release_acceptance_ephemeral)$",
    ),
    _admin=Depends(require_root_admin),
):
    try:
        from proxy.services.dataset_deletion_service import delete_datasets_safely
        from proxy.services.lexical_index_service import LexicalIndex
        return await delete_datasets_safely(
            dataset_ids=[dataset_id],
            qdrant_url=os.getenv("QDRANT_URL", "http://127.0.0.1:6333"),
            collection=rag_collection_name(),
            meta_db_path=rag_meta_db_path(),
            storage_root=mutable_path("./storage/datasets"),
            lexical_index=LexicalIndex(),
            recovery_policy=recovery_policy,
        )
    except KeyError as exc:
        raise HTTPException(status_code=404, detail=str(exc)) from exc
    except Exception as exc:
        logger.error("[DELETE] Dataset %s preserved after failed safe deletion: %s", dataset_id, exc)
        raise HTTPException(status_code=502, detail=str(exc)) from exc


@router.delete("/datasets")
async def delete_all_datasets(
    confirm: Annotated[str | None, Header(alias="X-LES-Confirm")] = None,
    _admin=Depends(require_root_admin),
):
    if confirm != "delete-all-datasets":
        raise HTTPException(
            status_code=409,
            detail="Bulk deletion requires X-LES-Confirm: delete-all-datasets",
        )
    backend = dataset_runtime.get_dataset_state().backend
    datasets = await backend.list_datasets()
    dataset_ids = [str(item.id) for item in datasets]
    if not dataset_ids:
        return {"status": "empty", "dataset_ids": []}
    try:
        from proxy.services.dataset_deletion_service import delete_datasets_safely
        from proxy.services.lexical_index_service import LexicalIndex
        result = await delete_datasets_safely(
            dataset_ids=dataset_ids,
            qdrant_url=os.getenv("QDRANT_URL", "http://127.0.0.1:6333"),
            collection=rag_collection_name(),
            meta_db_path=rag_meta_db_path(),
            storage_root=mutable_path("./storage/datasets"),
            lexical_index=LexicalIndex(),
        )
        result["status"] = "reset"
        return result
    except Exception as exc:
        logger.error("[DELETE] Bulk reset preserved catalog after failure: %s", exc)
        raise HTTPException(status_code=502, detail=str(exc)) from exc


@router.get("/datasets")
async def list_datasets(_user=Depends(require_user)):
    b = dataset_runtime.get_dataset_state().backend
    if not b:
        raise HTTPException(503, "Backend is initializing, please retry in a few seconds")
    return await b.list_datasets()


@router.get("/documents")
async def list_documents(
    dataset_id: str | None = None,
    status: str | None = Query(default=None, pattern="^(PENDING|INDEXED|ERROR|MISSING|SKIPPED)$"),
    q: str | None = Query(default=None, max_length=200),
    limit: int = Query(default=100, ge=1, le=5000),  # le=500 был тесен: диалог файлов датасета шлёт 1500
    offset: int = Query(default=0, ge=0),
    _user=Depends(require_user),
):
    # Keep direct unit-test calls usable; FastAPI replaces Query defaults at runtime.
    if not isinstance(dataset_id, str):
        dataset_id = None
    if not isinstance(status, str):
        status = None
    if not isinstance(q, str):
        q = None
    if not isinstance(limit, int):
        limit = 100
    if not isinstance(offset, int):
        offset = 0

    base_where = ["les_visible_document(doc.file_name)=1"]
    base_params: list[Any] = []
    if dataset_id:
        base_where.append("doc.dataset_id=?")
        base_params.append(dataset_id)
    q = q.strip() if q else None
    if q:
        pattern = f"%{q.lower()}%"
        base_where.append(
            "("
            "LOWER(doc.file_name) LIKE ? OR "
            "LOWER(COALESCE(ds.name, '')) LIKE ? OR "
            "LOWER(COALESCE(doc.domain, '')) LIKE ? OR "
            "LOWER(COALESCE(doc.route_dataset, '')) LIKE ? OR "
            "LOWER(COALESCE(doc.last_error, '')) LIKE ?"
            ")"
        )
        base_params.extend([pattern, pattern, pattern, pattern, pattern])
    row_where = list(base_where)
    row_params = list(base_params)
    if status:
        row_where.append("doc.status=?")
        row_params.append(status)
    summary_where_sql = "WHERE " + " AND ".join(base_where) if base_where else ""
    where_sql = "WHERE " + " AND ".join(row_where) if row_where else ""

    with sqlite3.connect(rag_meta_db_path()) as conn:
        conn.row_factory = sqlite3.Row
        from backend.product_edition import is_light
        from backend.smart_index import register_document_visibility
        register_document_visibility(conn, exclude_temporary=is_light())
        document_columns = {
            str(row["name"])
            for row in conn.execute("PRAGMA table_info(documents)").fetchall()
        }
        error_code_sql = "COALESCE(doc.error_code, '')" if "error_code" in document_columns else "''"
        retryable_sql = "COALESCE(doc.retryable, 0)" if "retryable" in document_columns else "0"
        attempts_sql = "COALESCE(doc.parse_attempts, 0)" if "parse_attempts" in document_columns else "0"
        retry_after_sql = "COALESCE(doc.retry_after, 0)" if "retry_after" in document_columns else "0"
        summary_rows = conn.execute(
            f"""
            SELECT doc.status AS status, COUNT(*) AS files, COALESCE(SUM(doc.chunk_count),0) AS chunks
            FROM documents doc
            LEFT JOIN datasets ds ON ds.id = doc.dataset_id
            {summary_where_sql}
            GROUP BY doc.status
            """,
            base_params,
        ).fetchall()
        total = conn.execute(
            f"""
            SELECT COUNT(*)
            FROM documents doc
            LEFT JOIN datasets ds ON ds.id = doc.dataset_id
            {where_sql}
            """,
            row_params,
        ).fetchone()[0]
        rows = conn.execute(
            f"""
            SELECT
                doc.id,
                doc.dataset_id,
                COALESCE(ds.name, '') AS dataset_name,
                doc.file_name,
                doc.status,
                COALESCE(doc.file_size, 0) AS file_size,
                COALESCE(doc.chunk_count, 0) AS chunk_count,
                COALESCE(doc.domain, '') AS domain,
                COALESCE(doc.route_dataset, '') AS route_dataset,
                COALESCE(doc.doc_type, '') AS doc_type,
                COALESCE(doc.content_type, '') AS content_type,
                COALESCE(doc.complexity, '') AS complexity,
                COALESCE(doc.pipeline, '') AS pipeline,
                COALESCE(doc.source_path, '') AS source_path,
                COALESCE(doc.last_error, '') AS last_error,
                {error_code_sql} AS error_code,
                {retryable_sql} AS retryable,
                {attempts_sql} AS parse_attempts,
                {retry_after_sql} AS retry_after
            FROM documents doc
            LEFT JOIN datasets ds ON ds.id = doc.dataset_id
            {where_sql}
            ORDER BY
                CASE doc.status
                    WHEN 'ERROR' THEN 0
                    WHEN 'INDEXED' THEN 1
                    WHEN 'PENDING' THEN 2
                    WHEN 'SKIPPED' THEN 3
                    ELSE 3
                END,
                doc.chunk_count DESC,
                doc.file_name
            LIMIT ? OFFSET ?
            """,
            [*row_params, limit, offset],
        ).fetchall()

    documents = [dict(row) for row in rows]
    dataset_ids_for_cards = sorted(
        {str(doc.get("dataset_id") or "") for doc in documents if doc.get("dataset_id")}
    )
    file_cards = latest_file_cards(dataset_ids_for_cards, meta_db_path=str(rag_meta_db_path()))
    for doc in documents:
        card = file_cards.get((str(doc.get("dataset_id") or ""), str(doc.get("file_name") or ""))) or {}
        doc["file_kind"] = str(card.get("file_kind") or "")
        doc["content_layers"] = list(card.get("content_layers") or [])
        doc["document_role"] = str(card.get("document_role") or "")
        doc["card_confidence"] = float(card.get("confidence") or 0.0) if card else 0.0

    return {
        "total": total,
        "limit": limit,
        "offset": offset,
        "summary": {
            row["status"]: {"files": row["files"], "chunks": row["chunks"]}
            for row in summary_rows
        },
        "documents": documents,
    }


def _validated_dataset_name(value: str) -> str:
    name = (value or "").strip()
    if not name:
        raise HTTPException(400, "Введите название датасета.")
    if len(name) > 120:
        raise HTTPException(400, "Название датасета должно содержать не более 120 символов.")
    if any(ord(char) < 32 or ord(char) == 127 for char in name):
        raise HTTPException(400, "Название датасета не должно содержать управляющие символы.")
    return name


@router.post("/datasets")
async def create_dataset(
    name: str = "",
    req: CreateDatasetRequest | None = None,
    _admin=Depends(require_admin),
):
    clean_name = _validated_dataset_name(req.name if req is not None and "name" in req.model_fields_set else name)
    state = dataset_runtime.get_dataset_state()
    ds_id = await state.backend.create_dataset(clean_name)
    return {"id": ds_id, "name": clean_name}


@router.patch("/datasets/{dataset_id}/sensitivity")
async def set_dataset_sensitivity(dataset_id: str, sensitivity: str, _admin=Depends(require_admin)):
    """W3.3 (ADR-9): пометить чувствительность датасета — P0 local-only / P1 / P2."""
    level = (sensitivity or "").strip().upper()
    if level not in ("P0", "P1", "P2"):
        raise HTTPException(400, "sensitivity must be P0, P1 or P2")
    await dataset_runtime.get_dataset_state().backend.set_dataset_sensitivity(dataset_id, level)
    return {"id": dataset_id, "sensitivity": level}


@router.patch("/datasets/{dataset_id}/group")
async def set_dataset_group(dataset_id: str, group: str = "", payload: DatasetGroupPayload | None = None, _user=Depends(require_user)):
    """Пользовательская группа датасета — организация списка в САМОВАРе (на поиск не влияет)."""
    raw = (payload.group if payload and payload.group else (payload.group_name if payload and payload.group_name else group))
    grp = (raw or "").strip()[:60]
    await dataset_runtime.get_dataset_state().backend.set_dataset_group(dataset_id, grp)
    return {"id": dataset_id, "group_name": grp}


@router.patch("/datasets/{dataset_id}/name")
async def set_dataset_name(dataset_id: str, name: str = "", payload: DatasetNamePayload | None = None, _user=Depends(require_user)):
    """Переименование датасета."""
    nm = _validated_dataset_name(payload.name if payload is not None else name)
    await dataset_runtime.get_dataset_state().backend.set_dataset_name(dataset_id, nm)
    return {"id": dataset_id, "name": nm}


@router.get("/datasets/{dataset_id}/profile")
async def dataset_context_profile(dataset_id: str, depth: str = "deep", _user=Depends(require_user)):
    """Паспорт датасета: состав, покрытие и путь к sidecar-файлу."""
    return get_dataset_profile(dataset_id, storage_root=mutable_path("storage/datasets"), depth=depth)


@router.post("/datasets/{dataset_id}/profile/refresh")
async def refresh_dataset_context_profile(dataset_id: str, depth: str = "deep", _admin=Depends(require_admin)):
    """Пересобрать паспорт датасета и записать sidecar рядом с датасетом."""
    return build_dataset_profile(dataset_id, storage_root=mutable_path("storage/datasets"), force=True, depth=depth)


@router.patch("/datasets/{dataset_id}/profile/guidance")
async def update_dataset_operator_guidance(
    dataset_id: str,
    req: DatasetGuidanceRequest,
    _admin=Depends(require_admin),
):
    """Сохранить комментарий оператора для модели. Навигация, не evidence."""
    return set_dataset_operator_guidance(
        dataset_id,
        req.guidance,
        storage_root=mutable_path("storage/datasets"),
        depth=req.depth,
    )


@router.patch("/datasets/{dataset_id}/profile/kind")
async def update_dataset_kind(
    dataset_id: str,
    req: DatasetKindRequest,
    _admin=Depends(require_admin),
):
    """Сохранить ручной тип датасета для сортировки и группировки операторского списка."""
    return set_dataset_kind(
        dataset_id,
        req.kind,
        storage_root=mutable_path("storage/datasets"),
        depth=req.depth,
    )


@router.post("/datasets/profiles/warmup")
async def warmup_dataset_context_profiles(req: DatasetProfileWarmupRequest, _admin=Depends(require_admin)):
    """Прогреть паспорта датасетов. No-reindex: читает только MetaDB/lexical index."""
    return warmup_dataset_profiles(
        dataset_ids=req.dataset_ids,
        storage_root=mutable_path("storage/datasets"),
        depth=req.depth,
        force=req.force,
        limit=req.limit,
    )


@router.post("/datasets/profiles/benchmark")
async def benchmark_dataset_context_profiles(req: DatasetProfileWarmupRequest, _admin=Depends(require_admin)):
    """Сравнить холодную пересборку deep-паспорта и тёплое чтение кэша. No-reindex."""
    return benchmark_dataset_profile_warmup(
        dataset_ids=req.dataset_ids,
        storage_root=mutable_path("storage/datasets"),
        depth=req.depth,
        limit=req.limit,
    )


@router.get("/sources")
async def list_sources(_user=Depends(require_user)):
    state = dataset_runtime.get_dataset_state()
    base_dir = Path("./RAG_Content")
    sources = []
    if base_dir.exists():
        ds_list = await state.backend.list_datasets()
        for folder in sorted(base_dir.iterdir()):
            if folder.is_dir() and not UUID_RE.match(folder.name) and folder.name not in SKIP_DIRS:
                src_files = [path for path in folder.rglob("*") if should_index_source_file(path, base_dir)]
                if not src_files:
                    continue
                ds_name = f"{folder.name}_Index"
                ds = next((dataset for dataset in ds_list if dataset.name == ds_name), None)
                sources.append(
                    {
                        "folder": folder.name,
                        "source_files": len(src_files),
                        "dataset_id": ds.id if ds else None,
                        "dataset_status": ds.status if ds else "NOT_CREATED",
                        "indexed_files": getattr(ds, "indexed_files", getattr(ds, "doc_count", 0)) if ds else 0,
                        "pending_files": getattr(ds, "pending_files", 0) if ds else 0,
                        "error_files": getattr(ds, "error_files", 0) if ds else 0,
                        "missing_files": getattr(ds, "missing_files", 0) if ds else 0,
                        "chunk_count": getattr(ds, "chunk_count", 0) if ds else 0,
                    }
                )
    return sources
