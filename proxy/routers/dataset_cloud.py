"""Dataset cloud endpoints."""
from __future__ import annotations
import asyncio
import logging
import os
from pathlib import Path
from typing import Any
from fastapi import APIRouter, Depends, HTTPException
from backend.interface import DatasetInfo
from proxy.security import require_admin
from proxy.services.cloud_drive_service import (
    CloudDriveError,
    cloud_drive_provider_status,
    discover_cloud_drive_roots,
    list_cloud_drive_folder,
    sync_cloud_drive_folder,
)
from proxy.storage.file_storage import validate_external_source

from proxy.services.dataset_contracts import (CloudDriveListRequest, CloudDriveSyncRequest, IndexExternalRequest)

import proxy.routers.dataset_external as dataset_external
import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])


@router.get("/cloud-drives")
async def cloud_drives(_admin=Depends(require_admin)):
    """Cloud drive integrations available for dataset intake."""
    roots = discover_cloud_drive_roots()
    return {
        "status": "ok",
        "providers": cloud_drive_provider_status(),
        "local_sync_roots": roots,
        "local_sync_count": len(roots),
        "mirror_root": os.getenv("LES_CLOUD_DRIVE_MIRROR_ROOT", "storage/cloud_drives"),
        "note": "Web-доступ использует OAuth-токены из env; локальные sync-папки остаются fallback.",
    }


@router.post("/cloud-drives/list")
async def cloud_drive_list(req: CloudDriveListRequest, _admin=Depends(require_admin)):
    try:
        return await asyncio.to_thread(
            list_cloud_drive_folder,
            req.provider,
            req.locator,
            limit=req.limit,
        )
    except CloudDriveError as exc:
        raise HTTPException(400, str(exc)) from exc


@router.post("/cloud-drives/sync")
async def cloud_drive_sync(req: CloudDriveSyncRequest, _admin=Depends(require_admin)):
    state = dataset_runtime.get_dataset_state()
    ds_list = await state.backend.list_datasets()
    dataset = None
    if req.dataset_id:
        dataset = next((item for item in ds_list if item.id == req.dataset_id), None)
        if dataset is None:
            raise HTTPException(404, f"dataset_id не найден: {req.dataset_id}")
    else:
        if not req.dataset_name.strip():
            raise HTTPException(400, "нужно указать dataset_id или dataset_name")
        ds_id = await state.backend.create_dataset(req.dataset_name.strip())
        dataset = DatasetInfo(id=ds_id, name=req.dataset_name.strip(), status="IDLE", doc_count=0, chunk_count=0)

    if req.background:
        asyncio.create_task(_cloud_drive_sync_run(state, req, dataset))
        return {
            "status": "started",
            "provider": req.provider,
            "dataset_id": dataset.id,
            "dataset_name": dataset.name,
            "note": "облачная папка синхронизируется в фоне, затем будет зарегистрирована как датасет",
        }
    return await _cloud_drive_sync_run(state, req, dataset)


async def _cloud_drive_sync_run(state, req: CloudDriveSyncRequest, dataset: DatasetInfo) -> dict[str, Any]:
    try:
        sync = await asyncio.to_thread(
            sync_cloud_drive_folder,
            req.provider,
            req.locator,
            dataset_name=dataset.name,
            max_files=req.max_files,
            max_depth=req.max_depth,
        )
    except CloudDriveError as exc:
        raise HTTPException(400, str(exc)) from exc
    local_path = str(sync.get("local_path") or "")
    if not local_path:
        raise HTTPException(500, "cloud sync did not return local_path")
    if int(sync.get("downloaded_count") or 0) <= 0:
        return {
            "status": "empty",
            "provider": req.provider,
            "dataset_id": dataset.id,
            "dataset_name": dataset.name,
            "sync": sync,
        }
    index_req = IndexExternalRequest(
        path=local_path,
        dataset_id=dataset.id,
        parse=req.parse,
        parse_limit=req.parse_limit,
        background=False,
    )
    indexed = await dataset_external._index_external_run(state, index_req, Path(local_path), dataset)
    return {
        "status": "registered",
        "provider": req.provider,
        "dataset_id": dataset.id,
        "dataset_name": dataset.name,
        "sync": sync,
        "index": indexed,
    }


@router.get("/browse-external")
async def browse_external(path: str = "", _admin=Depends(require_admin)):
    """Серверный браузер папок для выбора внешней папки кликами (без печати пути).

    По умолчанию (LES_EXTERNAL_ALLOW_ANY, single-user) — браузер ходит по ВСЕЙ локальной ФС от
    $HOME, любая папка индексируема. Строгий режим (=0) ограничивает корнями LES_EXTERNAL_SOURCE_ROOTS.
    Пустой `path` → старт ($HOME + корни как быстрый выбор); иначе — подпапки + «вверх».
    """
    from proxy.config import external_source_roots, external_allow_any, external_browse_default

    roots = external_source_roots()
    cloud_roots = discover_cloud_drive_roots()
    allow_any = external_allow_any()
    if not roots and not allow_any:
        raise HTTPException(403, "внешняя индексация выключена: LES_EXTERNAL_SOURCE_ROOTS пуст")

    if not (path or "").strip():
        cloud_dirs = [
            {
                "name": str(item.get("label") or item.get("provider_title") or "Облачный диск"),
                "path": str(item.get("path") or ""),
                "file_count": dataset_external._count_dir_files(Path(str(item.get("path") or ""))),
                "source": "cloud_drive",
                "provider": item.get("provider"),
                "provider_title": item.get("provider_title"),
            }
            for item in cloud_roots
            if item.get("is_dir") and item.get("path")
        ]
        if allow_any:
            start = external_browse_default()      # $HOME — отсюда видно любую папку
            dirs = []
            try:
                for child in sorted(start.iterdir(), key=lambda p: p.name.lower()):
                    if child.is_dir() and not child.name.startswith("."):
                        dirs.append({"name": child.name, "path": str(child), "file_count": dataset_external._count_dir_files(child)})
            except OSError:
                pass
            known = {item["path"] for item in cloud_dirs}
            dirs = cloud_dirs + [item for item in dirs if item.get("path") not in known]
            return {"path": str(start), "parent": str(start.parent) if start.parent != start else None,
                    "roots": [str(r) for r in roots], "cloud_roots": cloud_roots, "dirs": dirs}
        known = {item["path"] for item in cloud_dirs}
        root_dirs = [
            {"name": r.name or str(r), "path": str(r), "file_count": dataset_external._count_dir_files(r)}
            for r in roots
            if str(r) not in known
        ]
        return {"path": "", "parent": None, "roots": [str(r) for r in roots],
                "cloud_roots": cloud_roots, "dirs": cloud_dirs + root_dirs}

    current = validate_external_source(path)  # resolve+isdir guard (+ allowlist если строгий режим)
    dirs = []
    for child in sorted(current.iterdir(), key=lambda p: p.name.lower()):
        try:
            if child.is_dir() and not child.name.startswith("."):
                dirs.append({"name": child.name, "path": str(child), "file_count": dataset_external._count_dir_files(child)})
        except OSError:
            continue
    # в allow_any можно подниматься до корня ФС; в строгом — не выше одобренного корня.
    is_root = any(current == r for r in roots)
    parent = None if (current.parent == current or (not allow_any and is_root)) else str(current.parent)
    return {"path": str(current), "parent": parent, "roots": [str(r) for r in roots], "dirs": dirs}
