"""Dataset document ops endpoints."""
from __future__ import annotations
import asyncio
import logging
from fastapi import APIRouter, Depends, HTTPException, Query
from proxy.security import require_admin, require_user

from proxy.services.dataset_contracts import (_EXTRACT_STORAGE_ROOT)

import proxy.routers.dataset_parse as dataset_parse
import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])


@router.get("/datasets/{dataset_id}/extraction-status")
async def extraction_status_endpoint(dataset_id: str, _admin=Depends(require_admin)):
    """Read-only: что можно извлечь, есть ли sidecar/manifest/stale, OCR, extraction-state. Без записи."""
    from proxy.services import sidecar_ops_service as ops
    return ops.extraction_status(dataset_id, storage_root=_EXTRACT_STORAGE_ROOT)


@router.post("/datasets/{dataset_id}/repair")
async def repair_dataset(dataset_id: str, _admin=Depends(require_admin)):
    """Repair failed and encoding-damaged documents, then start their reindex job."""
    backend = dataset_runtime.get_dataset_state().backend
    error_count = await asyncio.to_thread(backend.db.requeue_error_documents, dataset_id)
    encoding_documents = await asyncio.to_thread(
        backend.db.requeue_corrupt_pdf_text_documents, dataset_id
    )
    requeued = error_count + len(encoding_documents)
    parse_job = None
    if requeued:
        backend.db.update_dataset_status(dataset_id, "IDLE")
        parse_job = await dataset_parse.parse_dataset_batch(
            dataset_id,
            limit=min(25, requeued),
            background=True,
            _admin=_admin,
        )
    return {
        "id": dataset_id,
        "requeued": requeued,
        "errors_requeued": error_count,
        "encoding_requeued": len(encoding_documents),
        "encoding_documents": encoding_documents,
        "job_id": (parse_job or {}).get("job_id"),
        "hint": "ремонт запущен" if requeued else "повреждений не найдено",
    }


@router.get("/datasets/{dataset_id}/integrity")
async def dataset_integrity(dataset_id: str, _admin=Depends(require_admin)):
    """Operator-facing exact audit; it does not mutate the dataset."""
    backend = dataset_runtime.get_dataset_state().backend
    return await asyncio.to_thread(backend.audit_dataset_integrity, dataset_id)


@router.post("/datasets/{dataset_id}/integrity/repair")
async def repair_dataset_integrity(dataset_id: str, _admin=Depends(require_admin)):
    """Repair only failed integrity components and start a bounded visible parse job."""
    backend = dataset_runtime.get_dataset_state().backend
    result = await asyncio.to_thread(backend.audit_dataset_integrity, dataset_id, repair=True)
    parse_job = None
    requeued = int(result.get("requeued") or 0)
    if requeued:
        backend.db.update_dataset_status(dataset_id, "IDLE")
        parse_job = await dataset_parse.parse_dataset_batch(
            dataset_id,
            limit=min(25, requeued),
            background=True,
            _admin=_admin,
        )
    result["job_id"] = (parse_job or {}).get("job_id")
    file_word = (
        "файл"
        if requeued % 10 == 1 and requeued % 100 != 11
        else "файла"
        if requeued % 10 in {2, 3, 4} and requeued % 100 not in {12, 13, 14}
        else "файлов"
    )
    result["label"] = (
        f"Исправление запущено: {requeued} {file_word}"
        if requeued
        else ("Датасет цел" if result.get("state") == "healthy" else str(result.get("label") or ""))
    )
    return result


@router.post("/datasets/{dataset_id}/reconcile")
async def reconcile_dataset_endpoint(dataset_id: str, _admin=Depends(require_admin)):
    """РЕКОНСАЙЛ MetaDB↔Qdrant: сверяет точки каждого INDEXED-документа; рассинхронные → PENDING
    (переиндексация чинит). Лечит тихую потерю recall от сбоев cleanup/краша. Дорогая операция-ремонт."""
    backend = dataset_runtime.get_dataset_state().backend
    res = await asyncio.to_thread(backend.reconcile_dataset, dataset_id)
    if res.get("requeued"):
        backend.db.update_dataset_status(dataset_id, "IDLE")
    res["hint"] = ("нажмите Пуск для переиндексации рассинхронных" if res.get("requeued")
                   else "рассинхрона нет — индекс согласован")
    return res


@router.post("/datasets/{dataset_id}/extract-body/dry-run")
async def extract_body_dry_run(dataset_id: str, _admin=Depends(require_admin)):
    """Dry-run извлечения: сколько файлов/абзацев/строк извлечётся. Ничего не пишет, оригиналы целы."""
    from proxy.services import sidecar_ops_service as ops
    return ops.extract_body_op(dataset_id, storage_root=_EXTRACT_STORAGE_ROOT, write=False)


@router.post("/datasets/{dataset_id}/extract-body/write")
async def extract_body_write(dataset_id: str, confirm_runtime_write: bool = False,
                             _admin=Depends(require_admin)):
    """Запись sidecar — ТОЛЬКО при confirm_runtime_write=true И env LES_ALLOW_RUNTIME_SIDECAR_WRITE=1
    (гейт внутри). Без env → blocked-ответ (dry-run). Оригиналы не меняются, пишутся только _extracted/."""
    from proxy.services import sidecar_ops_service as ops
    rep = ops.extract_body_op(dataset_id, storage_root=_EXTRACT_STORAGE_ROOT, write=True,
                              confirm_runtime_write=confirm_runtime_write)
    # blocked → 200 с самоописывающим отчётом (write_blocked + wrote_sidecars=0 + dry_run=True),
    # GUI показывает причину и следующее действие; оригиналы не тронуты в любом случае.
    return rep


@router.get("/datasets/{dataset_id}/pdf-extract/status")
async def pdf_extract_status_endpoint(dataset_id: str, _admin=Depends(require_admin)):
    """Read-only статус PDF source-map: есть ли sidecar, stale и покрытие. Без reindex."""
    from proxy.services.project_pdf_extract_service import project_pdf_extract_status

    return await asyncio.to_thread(
        project_pdf_extract_status,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.post("/datasets/{dataset_id}/pdf-extract/run")
async def pdf_extract_run_endpoint(
    dataset_id: str,
    force: bool = False,
    max_files: int = Query(80, ge=1, le=500),
    max_pages: int = Query(260, ge=1, le=2000),
    _admin=Depends(require_admin),
):
    """Построить project_pdf_extract_v1 sidecars. Пишет только _les_pdf_extract, индекс не трогает."""
    from proxy.services.project_pdf_extract_service import run_project_pdf_extract

    return await asyncio.to_thread(
        run_project_pdf_extract,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
        max_files=max_files,
        max_pages=max_pages,
        force=force,
    )


@router.get("/datasets/{dataset_id}/pdf-extract/summary")
async def pdf_extract_summary_endpoint(dataset_id: str, _admin=Depends(require_admin)):
    """Последняя PDF source-map summary для датасета. Missing возвращается как 200 с warnings."""
    from proxy.services.project_pdf_extract_service import project_pdf_extract_summary

    return await asyncio.to_thread(
        project_pdf_extract_summary,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.post("/datasets/{dataset_id}/table-registry/build")
async def table_registry_build_endpoint(dataset_id: str, _admin=Depends(require_admin)):
    """Build searchable Л.И.С.Т. table cards from sidecars; Qdrant is not changed."""
    from proxy.services.project_table_registry_service import build_project_table_registry

    return await asyncio.to_thread(
        build_project_table_registry,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.get("/datasets/{dataset_id}/table-registry/summary")
async def table_registry_summary_endpoint(dataset_id: str, _user=Depends(require_user)):
    from proxy.services.project_table_registry_service import project_table_registry_summary

    return await asyncio.to_thread(
        project_table_registry_summary,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.get("/datasets/{dataset_id}/tables/search")
async def table_registry_search_endpoint(
    dataset_id: str,
    q: str = Query(default="", max_length=1000),
    semantic_type: str = Query(default="", max_length=160),
    file: str = Query(default="", max_length=500),
    include_noise: bool = False,
    limit: int = Query(default=20, ge=1, le=100),
    _user=Depends(require_user),
):
    from proxy.services.project_table_registry_service import search_project_tables

    return await asyncio.to_thread(
        search_project_tables,
        dataset_id,
        q,
        semantic_type=semantic_type,
        file_filter=file,
        include_noise=include_noise,
        limit=limit,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.get("/datasets/{dataset_id}/tables/{table_id}")
async def table_registry_read_endpoint(
    dataset_id: str,
    table_id: str,
    max_rows: int = Query(default=100, ge=1, le=500),
    _user=Depends(require_user),
):
    from proxy.services.project_table_registry_service import read_project_table

    try:
        result = await asyncio.to_thread(
            read_project_table,
            dataset_id,
            table_id,
            max_rows=max_rows,
            storage_root=_EXTRACT_STORAGE_ROOT,
        )
        if result.get("status") == "stale":
            raise HTTPException(409, detail=result)
        return result
    except KeyError as exc:
        raise HTTPException(404, str(exc)) from exc
    except (ValueError, FileNotFoundError) as exc:
        raise HTTPException(400, str(exc)) from exc


@router.post("/datasets/{dataset_id}/document-registry/build")
async def document_registry_build_endpoint(dataset_id: str, _admin=Depends(require_admin)):
    """Classify dataset documents and group a virtual volume register by metadata."""
    from proxy.services.project_document_registry_service import build_project_document_registry

    return await asyncio.to_thread(
        build_project_document_registry,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.get("/datasets/{dataset_id}/document-registry")
async def document_registry_endpoint(dataset_id: str, _user=Depends(require_user)):
    from proxy.services.project_document_registry_service import project_document_registry

    return await asyncio.to_thread(
        project_document_registry,
        dataset_id,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.get("/datasets/{dataset_id}/virtual-volume")
async def virtual_volume_endpoint(
    dataset_id: str,
    index: str = Query(min_length=1, max_length=300),
    _user=Depends(require_user),
):
    from proxy.services.project_document_registry_service import assemble_virtual_volume

    return await asyncio.to_thread(
        assemble_virtual_volume,
        dataset_id,
        index,
        storage_root=_EXTRACT_STORAGE_ROOT,
    )


@router.get("/graph/edges")
async def graph_reference_edges(_user=Depends(require_user)):
    """W5.7-v2: рёбра «документ → документ» по упоминаниям номеров НТД (FTS, без LLM)."""
    import asyncio as _asyncio

    from proxy.services.graph_edges_service import build_reference_edges

    state = dataset_runtime.get_dataset_state()
    collection = getattr(state.backend, "collection_name", "")
    return await _asyncio.to_thread(build_reference_edges, collection)


@router.get("/graph/full")
async def graph_full(_user=Depends(require_user)):
    """Полный граф знаний: Проект→Датасет→Документ + NTD-ссылки, с метаданными узлов
    (для раскраски по проекту/датасету/типу/домену, размера по чанкам, клика→scope)."""
    import asyncio as _asyncio

    from proxy.services.graph_edges_service import build_graph_full
    from backend.product_edition import is_light

    state = dataset_runtime.get_dataset_state()
    collection = getattr(state.backend, "collection_name", "")
    return await _asyncio.to_thread(build_graph_full, collection, include_system=not is_light())
