"""Dataset watch endpoints."""
from __future__ import annotations
import asyncio
import copy
import logging
import sqlite3
import threading
import time
from pathlib import Path
from typing import Any
from fastapi import APIRouter, Depends, HTTPException
from backend.rag_config import rag_meta_db_path
from backend.smart_index import build_smart_plan
from proxy.security import require_admin, require_user

from proxy.services.dataset_contracts import (FOLDER_WATCH_CACHE_SAMPLE_LIMIT, FOLDER_WATCH_CACHE_TTL_SEC, FolderWatchRequest, SmartSyncRequest)

import proxy.services.dataset_parse_service as dataset_scheduler
import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])


_folder_watch_cache_lock = threading.Lock()


_folder_watch_cache: dict[str, tuple[float, dict[str, Any], list[dict[str, Any]]]] = {}


@router.get("/smart-plan")
async def smart_plan(details: bool = False, _user=Depends(require_user)):
    root = Path("./RAG_Content")
    if not root.exists():
        raise HTTPException(status_code=404, detail=f"source root not found: {root}")
    plan = await asyncio.to_thread(build_smart_plan, root)
    if details:
        return plan
    return {key: value for key, value in plan.items() if key not in {"plan", "rejected"}}


def _safe_source_root(source_root: str) -> Path:
    root = Path(source_root)
    if root.is_absolute() or ".." in root.parts:
        raise HTTPException(status_code=400, detail=f"unsafe source root: {source_root}")
    if not root.exists():
        raise HTTPException(status_code=404, detail=f"source root not found: {root}")
    return root


def _known_docs_inventory() -> dict[str, dict[Any, dict[str, Any]]]:
    rows: list[dict[str, Any]] = []
    try:
        with sqlite3.connect(rag_meta_db_path()) as conn:
            conn.row_factory = sqlite3.Row
            rows = [
                dict(row)
                for row in conn.execute(
                    """
                    SELECT d.name AS dataset_name,
                           d.id AS dataset_id,
                           doc.id AS doc_id,
                           doc.file_name,
                           doc.status,
                           COALESCE(doc.file_mtime, 0) AS file_mtime,
                           COALESCE(doc.file_size, 0) AS file_size,
                           COALESCE(doc.chunk_count, 0) AS chunk_count,
                           COALESCE(doc.last_error, '') AS last_error
                    FROM documents doc
                    JOIN datasets d ON d.id=doc.dataset_id
                    """
                ).fetchall()
            ]
    except sqlite3.Error:
        rows = []
    by_dataset_path = {(row["dataset_name"], row["file_name"]): row for row in rows}
    by_path: dict[str, dict[str, Any]] = {}
    by_basename: dict[str, dict[str, Any]] = {}
    basename_counts: dict[str, int] = {}
    for row in rows:
        file_name = str(row.get("file_name") or "")
        if file_name:
            by_path.setdefault(file_name, row)
            basename = Path(file_name).name
            basename_counts[basename] = basename_counts.get(basename, 0) + 1
            by_basename.setdefault(basename, row)
    by_basename = {
        basename: row
        for basename, row in by_basename.items()
        if basename_counts.get(basename) == 1
    }
    return {
        "by_dataset_path": by_dataset_path,
        "by_path": by_path,
        "by_basename": by_basename,
    }


def _folder_watch_inventory(root: Path, *, limit: int = 20) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    plan = build_smart_plan(root)
    known = _known_docs_inventory()
    samples: list[dict[str, Any]] = []
    changes: list[dict[str, Any]] = []
    counts = {"new": 0, "changed": 0, "route_changed": 0, "unchanged": 0}
    for dataset_name, items in plan["plan"].items():
        for item in items:
            key = (dataset_name, item["relative_path"])
            current = known["by_dataset_path"].get(key)
            if current is None:
                path_current = known["by_path"].get(item["relative_path"])
                basename_current = known["by_basename"].get(Path(item["relative_path"]).name)
                if path_current is not None:
                    current = path_current
                elif basename_current is not None and basename_current.get("dataset_name") == dataset_name:
                    current = basename_current
            state = "new"
            if current:
                size_changed = int(current.get("file_size") or 0) != int(item.get("size_bytes") or 0)
                try:
                    mtime_changed = abs(float(current.get("file_mtime") or 0) - Path(item["path"]).stat().st_mtime) > 1.0
                except OSError:
                    mtime_changed = False
                route_changed = current.get("dataset_name") != dataset_name
                if route_changed:
                    state = "route_changed"
                else:
                    state = "changed" if size_changed or mtime_changed else "unchanged"
            counts[state] += 1
            if state != "unchanged" and len(samples) < limit:
                samples.append(
                    {
                        "state": state,
                        "dataset_name": dataset_name,
                        "relative_path": item["relative_path"],
                        "size_bytes": item.get("size_bytes", 0),
                        "route": item.get("route", {}),
                        "current": current,
                    }
                )
            if state != "unchanged":
                changes.append(
                    {
                        "state": state,
                        "dataset_name": dataset_name,
                        "item": item,
                        "current": current,
                    }
                )
    return {
        "status": "ok",
        "source_root": root.as_posix(),
        "counts": counts,
        "pending_changes": counts["new"] + counts["changed"] + counts["route_changed"],
        "samples": samples,
        "plan_summary": plan["datasets"],
        "errors": plan["errors"],
    }, changes


def _folder_watch_cache_key(root: Path) -> str:
    try:
        return root.resolve().as_posix()
    except OSError:
        return root.as_posix()


def clear_folder_watch_cache(root: Path | None = None) -> None:
    with _folder_watch_cache_lock:
        if root is None:
            _folder_watch_cache.clear()
            return
        _folder_watch_cache.pop(_folder_watch_cache_key(root), None)


def _trim_folder_watch_status(status: dict[str, Any], *, limit: int) -> dict[str, Any]:
    result = copy.deepcopy(status)
    result["samples"] = list(result.get("samples") or [])[:limit]
    return result


def _folder_watch_inventory_cached(root: Path, *, limit: int = 20) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    key = _folder_watch_cache_key(root)
    now = time.monotonic()
    with _folder_watch_cache_lock:
        cached = _folder_watch_cache.get(key)
        if cached and now - cached[0] <= FOLDER_WATCH_CACHE_TTL_SEC:
            status, changes = cached[1], cached[2]
            return _trim_folder_watch_status(status, limit=limit), copy.deepcopy(changes)

        status, changes = _folder_watch_inventory(
            root,
            limit=max(limit, FOLDER_WATCH_CACHE_SAMPLE_LIMIT),
        )
        _folder_watch_cache[key] = (time.monotonic(), copy.deepcopy(status), copy.deepcopy(changes))
        return _trim_folder_watch_status(status, limit=limit), copy.deepcopy(changes)


def build_folder_watch_status(root: Path, *, limit: int = 20) -> dict[str, Any]:
    status, _changes = _folder_watch_inventory_cached(root, limit=limit)
    return status


def build_folder_reindex_plan(root: Path, *, limit: int = 50) -> dict[str, Any]:
    status, changes = _folder_watch_inventory_cached(root, limit=limit)
    route_changes = [change for change in changes if change["state"] == "route_changed"]
    groups: dict[tuple[str, str], dict[str, Any]] = {}
    samples: list[dict[str, Any]] = []
    for change in route_changes:
        item = change["item"]
        current = change.get("current") or {}
        target_dataset = str(change["dataset_name"])
        current_dataset = str(current.get("dataset_name") or "")
        key = (current_dataset, target_dataset)
        record = groups.setdefault(
            key,
            {
                "current_dataset_name": current_dataset,
                "current_dataset_id": current.get("dataset_id", ""),
                "target_dataset_name": target_dataset,
                "files": 0,
                "bytes": 0,
                "samples": [],
            },
        )
        doc = {
            "current_doc_id": current.get("doc_id", ""),
            "current_dataset_id": current.get("dataset_id", ""),
            "current_dataset_name": current_dataset,
            "target_dataset_name": target_dataset,
            "relative_path": item["relative_path"],
            "source_path": item["path"],
            "size_bytes": item.get("size_bytes", 0),
            "current_status": current.get("status", ""),
            "current_chunk_count": current.get("chunk_count", 0),
            "route": item.get("route", {}),
        }
        record["files"] += 1
        record["bytes"] += int(item.get("size_bytes") or 0)
        if len(record["samples"]) < 5:
            record["samples"].append(doc)
        if len(samples) < limit:
            samples.append(doc)

    return {
        "status": "ok",
        "source_root": root.as_posix(),
        "kind": "route_changed",
        "pending_route_changes": len(route_changes),
        "groups": sorted(
            groups.values(),
            key=lambda group: (-int(group["files"]), group["current_dataset_name"], group["target_dataset_name"]),
        ),
        "samples": samples,
        "watch_counts": status["counts"],
        "apply_supported": False,
        "safe_next_step": (
            "Use this as a dry-run plan. Route-change apply must delete old Qdrant points "
            "and move SQLite/storage rows under a guarded runner; ordinary watch scan skips it."
        ),
    }


@router.get("/watch/status")
async def folder_watch_status(source_root: str = "RAG_Content", limit: int = 20, _user=Depends(require_user)):
    root = _safe_source_root(source_root)
    return await asyncio.to_thread(build_folder_watch_status, root, limit=limit)


@router.get("/watch/reindex-plan")
async def folder_reindex_plan(source_root: str = "RAG_Content", limit: int = 50, _user=Depends(require_user)):
    root = _safe_source_root(source_root)
    return await asyncio.to_thread(build_folder_reindex_plan, root, limit=limit)


@router.post("/watch/scan")
async def folder_watch_scan(req: FolderWatchRequest, _admin=Depends(require_admin)):
    state = dataset_runtime.get_dataset_state()
    root = _safe_source_root(req.source_root)
    clear_folder_watch_cache(root)
    before, changes = await asyncio.to_thread(_folder_watch_inventory, root, limit=req.limit)
    route_changed = [change for change in changes if change["state"] == "route_changed"]
    register_changes = [change for change in changes if change["state"] in {"new", "changed"}]
    ds_list = await state.backend.list_datasets()
    dataset_ids = {dataset.name: dataset.id for dataset in ds_list}
    registered_by_dataset: dict[str, dict[str, Any]] = {}
    for change in register_changes:
        dataset_name = change["dataset_name"]
        item = change["item"]
        dataset_id = dataset_ids.get(dataset_name)
        if dataset_id is None:
            dataset_id = await state.backend.create_dataset(dataset_name)
            dataset_ids[dataset_name] = dataset_id
        await state.backend.upload_file(
            dataset_id,
            Path(item["path"]),
            relative_path=item["relative_path"],
        )
        record = registered_by_dataset.setdefault(
            dataset_name,
            {
                "dataset_id": dataset_id,
                "dataset_name": dataset_name,
                "pending_files": 0,
                "new": 0,
                "changed": 0,
                "route_changed": 0,
            },
        )
        record["pending_files"] += 1
        record[change["state"]] += 1
    after = await asyncio.to_thread(build_folder_watch_status, root, limit=req.limit)
    return {
        "status": "registered",
        "source_root": root.as_posix(),
        "before": before,
        "sync": {
            "status": "registered",
            "source_root": root.as_posix(),
            "datasets": list(registered_by_dataset.values()),
            "files": len(register_changes),
            "skipped_route_changed": len(route_changed),
            "parse_started": False,
            "parse_results": [],
            "plan_summary": before["plan_summary"],
            "errors": before["errors"],
        },
        "after": after,
    }


@router.post("/sync-smart")
async def sync_smart(req: SmartSyncRequest, _admin=Depends(require_admin)):
    state = dataset_runtime.get_dataset_state()
    root = _safe_source_root(req.source_root)

    plan = await asyncio.to_thread(build_smart_plan, root)
    ds_list = await state.backend.list_datasets()
    dataset_ids = {dataset.name: dataset.id for dataset in ds_list}
    registered = []
    total_files = 0
    for dataset_name, items in plan["plan"].items():
        dataset_id = dataset_ids.get(dataset_name)
        if dataset_id is None:
            dataset_id = await state.backend.create_dataset(dataset_name)
            dataset_ids[dataset_name] = dataset_id
        for item in items:
            await state.backend.upload_file(
                dataset_id,
                Path(item["path"]),
                relative_path=item["relative_path"],
            )
        registered.append({"dataset_id": dataset_id, "dataset_name": dataset_name, "pending_files": len(items)})
        total_files += len(items)

    parse_results = []
    if req.parse:
        async with state.sync_parse_semaphore:
            await dataset_scheduler.assert_parse_admission(state)
            for item in registered:
                result = await state.backend.parse_dataset(
                    item["dataset_id"],
                    limit=req.parse_limit_per_dataset,
                )
                parse_results.append({"dataset_id": item["dataset_id"], "dataset_name": item["dataset_name"], "result": result})

    return {
        "status": "registered",
        "source_root": root.as_posix(),
        "datasets": registered,
        "files": total_files,
        "parse_started": req.parse,
        "parse_results": parse_results,
        "plan_summary": plan["datasets"],
        "errors": plan["errors"],
    }
