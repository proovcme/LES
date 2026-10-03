"""Dataset external endpoints."""
from __future__ import annotations
import asyncio
import logging
import os
import re
import sqlite3
import threading
from datetime import datetime
from pathlib import Path
from backend.light_qdrant_connection import qdrant_http_headers
from typing import Any
import httpx
from fastapi import APIRouter, Depends, HTTPException
from backend.rag_config import rag_collection_name, rag_meta_db_path
from backend.smart_index import is_temporary_source_name
from proxy.config import rag_upload_suffixes
from proxy.security import require_admin
from proxy.storage.file_storage import is_within_external_root, validate_external_source

from proxy.services.dataset_contracts import (DEFAULT_PARSE_BATCH_LIMIT, DEFAULT_PARSE_DRAIN_MAX_BATCHES, EXTERNAL_SERVICE_FILENAMES, ExternalDatasetSyncRequest, ExternalIntakePlanRequest, IndexExternalRequest)
from proxy.services.dataset_runtime import DatasetRouterState

import proxy.services.dataset_parse_service as dataset_scheduler
import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])




def _project_name_from_dataset(dataset_name: str, explicit: str = "") -> str:
    value = (explicit or "").strip()
    if value:
        return value
    value = (dataset_name or "").strip()
    for suffix in ("_Проект", " Проект", "_Project", " Project", "_Index"):
        if value.endswith(suffix):
            value = value[: -len(suffix)]
            break
    return value.strip(" _-") or (dataset_name or "Проект").strip()


def _discipline_hints(name: str) -> list[str]:
    text = re.sub(r"[^A-ZА-Я0-9]+", "_", name.upper())
    parts = {part for part in text.split("_") if part}
    hints: list[str] = []
    for token in ("ЭОМ", "ЭО", "ИОС", "ОВ", "ВК", "АР", "КР", "СС", "АПС", "СОУЭ", "СКС", "ТМ"):
        if any(part == token or (token in {"ИОС", "ЭОМ"} and part.startswith(token)) for part in parts):
            hints.append(token)
    if "БЕСПЕРЕБО" in text and "ЭОМ" not in hints:
        hints.append("ЭОМ")
    return hints


def _document_role_hint(name: str) -> str:
    suffix = Path(name).suffix.casefold()
    if suffix in {".xlsx", ".xls", ".csv"}:
        return "таблица"
    if suffix in {".png", ".jpg", ".jpeg", ".tif", ".tiff"}:
        return "изображение"
    return "документ"


def _external_intake_plan(root: Path, *, dataset_name: str, project_name: str = "") -> dict[str, Any]:
    from backend.external_scan import source_files
    suffixes = rag_upload_suffixes()
    accepted: list[dict[str, Any]] = []
    skipped: list[dict[str, Any]] = []
    disciplines: set[str] = set()
    role_counts: dict[str, int] = {}
    bytes_total = 0

    for path in source_files(root):
        rel = path.relative_to(root).as_posix()
        if path.name in {"LES.md", "ЛЕС.md", "les.md", "лес.md", "00_dataset_map.md"}:
            continue
        try:
            size = path.stat().st_size
        except OSError as error:
            raise OSError(f'Не удалось прочитать сведения о файле: {rel}') from error
        if path.name.startswith('.') or is_temporary_source_name(path.name):
            skipped.append({"file_name": rel, "reason": "hidden/system"})
            continue
        if "_originals" in path.parts:
            skipped.append({"file_name": rel, "reason": "original_archive"})
            continue
        if not is_within_external_root(path, root):
            skipped.append({"file_name": rel, "reason": "outside_root"})
            continue
        suffix = path.suffix.lower()
        if suffix not in suffixes:
            skipped.append({"file_name": rel, "reason": "unsupported_suffix", "suffix": suffix, "size_bytes": size})
            continue

        role = _document_role_hint(path.name)
        for hint in _discipline_hints(path.name):
            disciplines.add(hint)
        role_counts[role] = role_counts.get(role, 0) + 1
        bytes_total += size
        accepted.append(
            {
                "file_name": rel,
                "suffix": suffix,
                "size_bytes": size,
                "role_hint": role,
                "discipline_hints": _discipline_hints(path.name),
            }
        )

    return {
        "status": "ok",
        "source_root": root.as_posix(),
        "project_name": _project_name_from_dataset(dataset_name, project_name),
        "dataset_name": dataset_name.strip(),
        "will_create": {
            "dataset": dataset_name.strip(),
        },
        "accepted_count": len(accepted),
        "source_state": 'documents' if accepted else ('unsupported_only' if skipped else 'empty'),
        "accepted_bytes": bytes_total,
        "skipped_count": len(skipped),
        "accepted": accepted[:200],
        "skipped": skipped[:200],
        "maps": [{"file_name": name, "status": "existing"} for name in ('LES.md', '00_dataset_map.md') if (root / name).is_file()],
        "disciplines": sorted(disciplines),
        "role_counts": dict(sorted(role_counts.items())),
        "warnings": [] if accepted else ["supported_documents_not_found"],
    }


@router.post("/external/intake-plan")
async def external_intake_plan(req: ExternalIntakePlanRequest, _admin=Depends(require_admin)):
    root = validate_external_source(req.path)
    try:
        return await asyncio.to_thread(_external_intake_plan, root, dataset_name=req.dataset_name, project_name=req.project_name)
    except OSError as error:
        raise HTTPException(403, 'Не удалось прочитать папку целиком. Проверьте права доступа; папка не считается пустой.') from error
    except ValueError as error:
        raise HTTPException(413, str(error)) from error


@router.post("/index-external")
async def index_external(req: IndexExternalRequest, _admin=Depends(require_admin)):
    """In-place индексация одобренной внешней папки.

    Исходники НЕ копируются в storage — в LES попадают только производные
    (Qdrant-векторы, Parquet, метаданные). Путь обязан быть внутри
    LES_EXTERNAL_SOURCE_ROOTS (resolve снимает симлинки → ссылка/`..` наружу
    отклоняется). Каждый файл дополнительно проверяется на выход за корень.
    """
    state = dataset_runtime.get_dataset_state()
    root = validate_external_source(req.path)

    ds_list = await state.backend.list_datasets()
    dataset = next((dataset for dataset in ds_list if dataset.id == req.dataset_id), None)
    if dataset is None:
        raise HTTPException(404, f"dataset_id не найден: {req.dataset_id} (создайте датасет заранее)")

    if req.background:
        asyncio.create_task(_index_external_run_safe(state, req, root, dataset))
        return {"status": "started", "dataset_id": req.dataset_id, "dataset_name": dataset.name,
                "note": "регистрация и индексация идут в фоне — файлы появятся в датасете"}
    return await _index_external_run(state, req, root, dataset)


async def _index_external_run_safe(state, req, root, dataset) -> dict:
    try:
        return await _index_external_run(state, req, root, dataset)
    except Exception as err:
        logger.error("[INDEX-EXT] Ошибка фонного индексирования %s: %s", req.dataset_id, err, exc_info=True)
        try:
            await state.backend.update_dataset_status(req.dataset_id, "ERROR")
        except Exception:
            pass
        return {"status": "error", "error": str(err)}


async def _index_external_run(state, req, root, dataset) -> dict:
    """Тело in-place индексации: нарезка крупных PDF + регистрация файлов + LES.md + (опц.) парс.
    Выносимо в фон (req.background) — не зависит от HTTP-таймаута на больших папках (758 файлов = ~47с)."""
    suffixes = rag_upload_suffixes()
    registered = 0
    skipped_unsupported = 0
    skipped_outside_root = 0
    samples: list[str] = []

    # 1. Сначала быстрая регистрация исходных файлов в SQLite: датасет мгновенно наполняется в UI.
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if "_originals" in path.parts:
            continue
        if not is_within_external_root(path, root):
            skipped_outside_root += 1
            continue
        resolved = path.resolve()
        if resolved.name in EXTERNAL_SERVICE_FILENAMES or is_temporary_source_name(resolved.name):
            skipped_unsupported += 1
            continue
        if resolved.suffix.lower() not in suffixes:
            skipped_unsupported += 1
            continue
        file_name = resolved.relative_to(root.parent).as_posix()
        await state.backend.register_external_file(req.dataset_id, resolved, file_name)
        registered += 1
        if registered % 20 == 0:
            await asyncio.sleep(0)
        if len(samples) < 10:
            samples.append(file_name)

    # External folders are read-only inputs. Preprocessing and service files
    # belong to application state, never next to the user's originals.
    split_summary = {"split_files": 0, "parts": 0}
    les_md_summary = None
    map_summary = None
    intake_plan = await asyncio.to_thread(_external_intake_plan, root, dataset_name=dataset.name)

    if registered == 0:
        raise HTTPException(400, f"в папке нет поддерживаемых документов: {root}")

    parse_started = False
    parse_job = None
    if req.parse:
        batch_limit = max(1, int(req.parse_limit or DEFAULT_PARSE_BATCH_LIMIT))
        max_batches = min(
            DEFAULT_PARSE_DRAIN_MAX_BATCHES,
            max(1, (registered + batch_limit - 1) // batch_limit),
        )
        job = state.job_service.create(
            "rag_parse_drain",
            source="external",
            dataset_id=req.dataset_id,
            dataset_name=dataset.name,
            status="queued",
            total=registered,
            message=f"Парсинг внешней папки: {dataset.name} · {registered} файлов",
        )
        job_id = job["id"]
        state.job_tracker[job_id] = {
            "id": job_id,
            "type": "rag_parse_drain",
            "status": "QUEUED",
            "source": "external",
            "dataset_id": req.dataset_id,
            "dataset_name": dataset.name,
            "total": registered,
            "processed": 0,
            "errors": 0,
            "started_at": job.get("started_at"),
            "message": f"Парсинг внешней папки: {dataset.name} · {registered} файлов",
        }

        async def _parse():
            try:
                await dataset_scheduler.run_dataset_parse_drain(
                    state,
                    dataset_id=req.dataset_id,
                    dataset_name=dataset.name,
                    batch_limit=batch_limit,
                    max_batches=max_batches,
                    job_id=job_id,
                    reason="index_external_drain",
                )
            except Exception as error:
                message = f"Ошибка парсинга внешней папки: {error}"
                state.job_tracker[job_id].update(
                    {
                        "status": "FAILED",
                        "errors": 1,
                        "finished_at": datetime.now().isoformat(),
                        "message": message,
                    }
                )
                state.job_service.update(job_id, status="failed", errors=1, message=message)
                logger.error("[INDEX-EXT PARSE %s] FAILED: %s", job_id, error, exc_info=True)

        asyncio.create_task(_parse())
        parse_started = True
        parse_job = {
            "job_id": job_id,
            "type": "rag_parse_drain",
            "batch_limit": batch_limit,
            "max_batches": max_batches,
        }

    return {
        "status": "registered",
        "source_root": root.as_posix(),
        "dataset_id": req.dataset_id,
        "dataset_name": dataset.name,
        "registered_files": registered,
        "skipped_unsupported": skipped_unsupported,
        "skipped_outside_root": skipped_outside_root,
        "split_large_pdfs": split_summary["split_files"],
        "split_parts": split_summary["parts"],
        "in_place": True,
        "copied_to_storage": False,
        "parse_started": parse_started,
        "parse_limit": req.parse_limit,
        "parse_job": parse_job,
        "samples": samples,
        "les_md": les_md_summary,  # auto-init: что ЛЕС сам понял о папке + директивы
        "dataset_map": map_summary,
        "intake_plan": intake_plan,
    }


def _external_supported_files(root: Path, *, max_files: int = 50000) -> dict[str, dict[str, Any]]:
    suffixes = rag_upload_suffixes()
    files: dict[str, dict[str, Any]] = {}
    def scan_error(error):
        raise error

    try:
        for directory, dirs, names in os.walk(root, onerror=scan_error, followlinks=False):
            dirs[:] = [name for name in dirs if name != "_originals" and is_within_external_root(Path(directory) / name, root)]
            for name in names:
                path = Path(directory) / name
                if is_temporary_source_name(name) or name in EXTERNAL_SERVICE_FILENAMES or path.suffix.lower() not in suffixes or not is_within_external_root(path, root):
                    continue
                resolved = path.resolve()
                stat = resolved.stat()
                file_name = resolved.relative_to(root.parent).as_posix()
                if len(files) >= max_files:
                    raise HTTPException(413, "В папке больше 50 000 поддерживаемых файлов. Разделите наблюдение на меньшие папки; синхронизация по неполному списку отменена.")
                files[file_name] = {
                    "file_name": file_name,
                    "source_path": str(resolved),
                    "size_bytes": int(stat.st_size),
                    "mtime": float(stat.st_mtime),
                }
    except OSError as error:
        raise HTTPException(409, "Не удалось полностью прочитать папку датасета. Проверьте доступ к файлам и повторите синхронизацию; удаление по неполному списку отменено.") from error
    return files


def _external_dataset_docs(dataset_id: str) -> dict[str, dict[str, Any]]:
    try:
        with sqlite3.connect(rag_meta_db_path()) as conn:
            conn.row_factory = sqlite3.Row
            rows = conn.execute(
                """
                SELECT id, file_name, status, COALESCE(file_mtime, 0) AS file_mtime,
                       COALESCE(file_size, 0) AS file_size, COALESCE(chunk_count, 0) AS chunk_count,
                       COALESCE(source_path, '') AS source_path, COALESCE(last_error, '') AS last_error,
                       COALESCE(file_hash, '') AS file_hash
                FROM documents
                WHERE dataset_id=? AND COALESCE(source_path, '') <> ''
                """,
                (dataset_id,),
            ).fetchall()
    except sqlite3.Error as error:
        raise HTTPException(503, "Реестр документов недоступен. Синхронизация отменена; повторите после восстановления хранилища.") from error
    return {str(row["file_name"]): dict(row) for row in rows}


def _external_dataset_diff(dataset_id: str, root: Path, *, limit: int = 50) -> dict[str, Any]:
    current = _external_supported_files(root)
    known = {
        name: row for name, row in _external_dataset_docs(dataset_id).items()
        if row.get("source_path") and Path(row["source_path"]).resolve().is_relative_to(root.resolve())
    }
    new: list[dict[str, Any]] = []
    changed: list[dict[str, Any]] = []
    deleted: list[dict[str, Any]] = []
    unchanged = 0
    for file_name, item in current.items():
        row = known.get(file_name)
        if row is None:
            new.append(item)
            continue
        size_changed = int(row.get("file_size") or 0) != int(item["size_bytes"])
        mtime_changed = float(row.get("file_mtime") or 0) != float(item["mtime"])
        missing_before = str(row.get("status") or "").upper() == "MISSING"
        content_changed = False
        if row.get("file_hash") and not size_changed and not mtime_changed:
            from backend.qdrant_adapter import _sha256_file
            try:
                digest = _sha256_file(Path(item["source_path"]))
            except OSError as error:
                raise HTTPException(409, "Не удалось проверить содержимое файла. Проверьте доступ и повторите синхронизацию; удаление по неполной проверке отменено.") from error
            item["file_hash"] = digest
            content_changed = digest != row["file_hash"]
        if size_changed or mtime_changed or missing_before or content_changed:
            if content_changed:
                item["content_changed"] = True
            changed.append({**item, "previous": row})
        else:
            unchanged += 1
    for file_name, row in known.items():
        source_path = str(row.get("source_path") or "")
        if str(row.get("status") or "").upper() == "MISSING":
            continue
        if file_name not in current and (not source_path or not Path(source_path).exists()):
            deleted.append(row)
    return {
        "status": "ok",
        "source_root": root.as_posix(),
        "dataset_id": dataset_id,
        "counts": {
            "new": len(new),
            "changed": len(changed),
            "deleted": len(deleted),
            "unchanged": unchanged,
            "known_external": len(known),
            "current_supported": len(current),
        },
        "pending_changes": len(new) + len(changed) + len(deleted),
        "samples": {
            "new": new[:limit],
            "changed": changed[:limit],
            "deleted": deleted[:limit],
        },
        "_files": {"new": new, "changed": changed, "deleted": deleted},
    }


def _mark_external_missing(dataset_id: str, rows: list[dict[str, Any]]) -> int:
    if not rows:
        return 0
    count = 0
    with sqlite3.connect(rag_meta_db_path()) as conn:
        for row in rows:
            cur = conn.execute(
                """
                UPDATE documents
                SET status='MISSING', chunk_count=0, last_error='source file missing', stage=''
                WHERE dataset_id=? AND file_name=?
                """,
                (dataset_id, str(row.get("file_name") or "")),
            )
            count += int(cur.rowcount or 0)
    return count


async def _delete_index_for_files(state: DatasetRouterState, dataset_id: str, file_names: list[str]) -> dict[str, Any]:
    if not file_names:
        return {"files": 0, "qdrant_deleted": 0, "lexical_deleted": 0, "errors": []}
    qdrant_url = os.getenv("QDRANT_URL", "http://127.0.0.1:6333")
    errors: list[str] = []
    qdrant_deleted = 0
    lexical_deleted = 0
    try:
        from proxy.services.lexical_index_service import LexicalIndex

        lexical = LexicalIndex()
    except Exception:
        lexical = None
    async with httpx.AsyncClient(timeout=20.0, headers=qdrant_http_headers(qdrant_url), trust_env=False) as client:
        for file_name in file_names:
            file_filter = {
                "must": [
                    {"key": "dataset_id", "match": {"value": dataset_id}},
                    {"key": "file_name", "match": {"value": file_name}},
                ]
            }
            try:
                response = await client.post(
                    f"{qdrant_url}/collections/{rag_collection_name()}/points/delete?wait=true",
                    json={"filter": file_filter},
                )
                response.raise_for_status()
                qdrant_deleted += 1
            except Exception as error:
                errors.append(f"Qdrant {file_name}: {error}")
            try:
                if lexical is not None:
                    lexical_deleted += int(
                        await asyncio.to_thread(
                            lexical.delete_file,
                            rag_collection_name(),
                            dataset_id=dataset_id,
                            doc_name=file_name,
                        )
                    )
            except Exception as error:
                errors.append(f"Lexical {file_name}: {error}")
    try:
        backend = state.backend
        if hasattr(backend, "db"):
            backend.db.update_dataset_chunk_count(dataset_id)
    except Exception as error:
        errors.append(f"dataset chunk count: {error}")
    return {
        "files": len(file_names),
        "qdrant_deleted": qdrant_deleted,
        "lexical_deleted": lexical_deleted,
        "errors": errors,
    }


@router.post("/external/check")
async def check_external_dataset(req: ExternalDatasetSyncRequest, _admin=Depends(require_admin)):
    root = validate_external_source(req.path)
    state = dataset_runtime.get_dataset_state()
    ds_list = await state.backend.list_datasets()
    dataset = next((dataset for dataset in ds_list if dataset.id == req.dataset_id), None)
    if dataset is None:
        raise HTTPException(404, f"dataset_id не найден: {req.dataset_id}")
    diff = await asyncio.to_thread(_external_dataset_diff, req.dataset_id, root, limit=req.limit)
    diff.pop("_files", None)
    diff["dataset_name"] = dataset.name
    return diff


@router.post("/external/sync")
async def sync_external_dataset(req: ExternalDatasetSyncRequest, _admin=Depends(require_admin)):
    root = validate_external_source(req.path)
    state = dataset_runtime.get_dataset_state()
    ds_list = await state.backend.list_datasets()
    dataset = next((dataset for dataset in ds_list if dataset.id == req.dataset_id), None)
    if dataset is None:
        raise HTTPException(404, f"dataset_id не найден: {req.dataset_id}")
    diff = await asyncio.to_thread(_external_dataset_diff, req.dataset_id, root, limit=req.limit)
    files = diff.pop("_files")
    registered = 0
    for item in [*files["new"], *files["changed"]]:
        await state.backend.register_external_file(req.dataset_id, Path(item["source_path"]), item["file_name"],
            **({"force_reindex": True} if item.get("content_changed") else {}))
        registered += 1
    deleted_rows = files["deleted"] if req.include_deleted else []
    cleanup = await _delete_index_for_files(
        state,
        req.dataset_id,
        [str(row.get("file_name") or "") for row in deleted_rows if row.get("file_name")],
    )
    if cleanup["errors"]:
        raise HTTPException(503, "Не удалось убрать устаревшие записи из индекса. Проверьте хранилище и повторите синхронизацию; удаление не отмечено как завершённое.")
    missing_marked = await asyncio.to_thread(_mark_external_missing, req.dataset_id, deleted_rows)
    parse_started = False
    parse_job = None
    if req.parse and registered:
        batch_limit = max(1, int(req.parse_limit or DEFAULT_PARSE_BATCH_LIMIT))
        max_batches = min(
            DEFAULT_PARSE_DRAIN_MAX_BATCHES,
            max(1, (registered + batch_limit - 1) // batch_limit),
        )
        job = state.job_service.create(
            "rag_parse_drain",
            source="external_sync",
            dataset_id=req.dataset_id,
            dataset_name=dataset.name,
            status="queued",
            total=registered,
            message=f"Парсинг изменений внешней папки: {dataset.name} · {registered} файлов",
        )
        job_id = job["id"]
        state.job_tracker[job_id] = {
            "id": job_id,
            "type": "rag_parse_drain",
            "status": "QUEUED",
            "source": "external_sync",
            "dataset_id": req.dataset_id,
            "dataset_name": dataset.name,
            "total": registered,
            "processed": 0,
            "errors": 0,
            "started_at": job.get("started_at"),
            "message": f"Парсинг изменений внешней папки: {dataset.name} · {registered} файлов",
        }

        async def _parse():
            try:
                await dataset_scheduler.run_dataset_parse_drain(
                    state,
                    dataset_id=req.dataset_id,
                    dataset_name=dataset.name,
                    batch_limit=batch_limit,
                    max_batches=max_batches,
                    job_id=job_id,
                    reason="external_sync_drain",
                )
            except Exception as error:
                message = f"Ошибка парсинга изменений внешней папки: {error}"
                state.job_tracker[job_id].update(
                    {
                        "status": "FAILED",
                        "errors": 1,
                        "finished_at": datetime.now().isoformat(),
                        "message": message,
                    }
                )
                state.job_service.update(job_id, status="failed", errors=1, message=message)
                logger.error("[EXT_SYNC PARSE %s] FAILED: %s", job_id, error, exc_info=True)

        asyncio.create_task(_parse())
        parse_started = True
        parse_job = {
            "job_id": job_id,
            "type": "rag_parse_drain",
            "batch_limit": batch_limit,
            "max_batches": max_batches,
        }
    return {
        "status": "synced",
        "source_root": root.as_posix(),
        "dataset_id": req.dataset_id,
        "dataset_name": dataset.name,
        "diff": diff,
        "registered": registered,
        "missing_marked": missing_marked,
        "cleanup": cleanup,
        "parse_started": parse_started,
        "parse_limit": req.parse_limit,
        "parse_job": parse_job,
    }


def _count_dir_files(d: Path) -> int:
    """Быстрый счёт файлов в папке (только непосредственные дети) — для подсказки в браузере."""
    try:
        return sum(1 for x in d.iterdir() if x.is_file() and not x.name.startswith("."))
    except OSError:
        return 0
