"""
С.О.В.У.Ш.К.А. v5.0 — Вкладка С.А.М.О.В.А.Р. (RAG-индекс)
"""
from __future__ import annotations

import asyncio
import json
import sys
from html import escape
from datetime import datetime
from urllib.parse import quote, urlencode
from nicegui import context, ui
from backend.product_edition import is_light
from sovushka.components.folder_setup import open_folder_setup, _dataset_name_from_path

from sovushka.config import UI_PORT
from sovushka.uikit.components import (
    acronym_identity,
    action_button,
    panel,
    render_feedback_state,
    section_heading,
    status_badge,
    text_field,
)
from sovushka.state import (
    state,
    api_get,
    api_post,
    api_put,
    api_patch,
    add_log,
    refresh_proxy_logs,
    refresh_samovar,
    last_api_error_text,
)

_LAYER_LABELS = {
    "text": "текст",
    "graphics": "графика",
    "tables": "таблицы",
    "calculations": "расчёты",
    "technical_docs": "техничка",
    "drawings": "чертежи",
    "cad_bim": "BIM",
    "normative": "нормы",
    "estimate": "сметы",
}

_DEFAULT_INDEX_SETTINGS = {
    "batch_limit": 1,
    "max_batches": 25,
    "cooldown_sec": 20,
    "min_free_gb": 8.0,
    "max_swap_pct": 45.0,
    "unload_between_batches": True,
    "unload_before_start": True,
    "row_batch_limit": 25,
}

_LOCAL_UI_BASE = f"http://127.0.0.1:{UI_PORT}"




def _doc_layer_labels(item: dict) -> list[str]:
    labels: list[str] = []
    for layer in item.get("content_layers") or []:
        label = _LAYER_LABELS.get(str(layer), str(layer))
        if label and label not in labels:
            labels.append(label)
    role = str(item.get("document_role") or "").strip()
    if role and role not in labels:
        labels.insert(0, role)
    kind = str(item.get("file_kind") or "").strip()
    if not labels and kind:
        labels.append(kind.replace("_", " "))
    content = str(item.get("content_type") or item.get("content") or "").strip()
    if not labels and content:
        labels.append(content)
    doc_type = str(item.get("doc_type") or "").strip()
    if not labels and doc_type:
        labels.append(doc_type)
    return labels[:5]


def _layer_counts(documents: list[dict]) -> list[tuple[str, int]]:
    counts: dict[str, int] = {}
    for item in documents:
        seen = set(_doc_layer_labels(item)[:4])
        if not seen:
            seen = {"без карточки"}
        for label in seen:
            counts[label] = counts.get(label, 0) + 1
    return sorted(counts.items(), key=lambda pair: (-pair[1], pair[0]))[:12]


def _computed_index_status(
    *,
    raw_status: str = "",
    total: int = 0,
    indexed: int = 0,
    pending: int = 0,
    errors: int = 0,
    missing: int = 0,
    active: bool = False,
) -> str:
    if active:
        return "PARSING"
    if errors:
        return "ERROR"
    if missing:
        return "MISSING"
    if pending:
        return "WAITING"
    if indexed or total:
        return "INDEXED"
    raw = str(raw_status or "").upper()
    if raw in {"PARSING", "SCANNING", "RUNNING"}:
        return "IDLE"
    return raw or "EMPTY"


def _operator_queue_notice(
    *, pending: int, last_status: str, last_message: str, contract_compatible: bool
) -> tuple[str, str] | None:
    """Return one current, actionable operator state instead of a stale job verdict."""
    if pending <= 0 or str(last_status).upper() not in {"FAILED", "ERROR", "CANCELLED"}:
        return None
    message = str(last_message or "").strip()
    if "index contract missing" in message.lower():
        if contract_compatible:
            return (f"ГОТОВ К ПРОДОЛЖЕНИЮ · {pending} файлов ждут · нажмите «Пуск»", "ready")
        message = "локальный индекс не подготовлен; перезапустите ЛЕС"
    return (f"ОСТАНОВЛЕНО · {pending} файлов ждут · {message}", "error")


def _dataset_source_label(row: dict) -> str:
    """Return a human source label without creating another navigation layer."""
    source = str(row.get("source_type") or "").casefold()
    kind = str(row.get("dataset_kind") or row.get("kind") or "").casefold()
    name = str(row.get("name") or "").upper()
    if (
        source in {"imap", "mail", "outlook"}
        or kind == "correspondence"
        or "_MAIL_" in name
        or name.startswith("MAIL_")
    ):
        return "Почта"
    if str(row.get("dataset_scope") or "").casefold() == "system":
        return "Служебные данные"
    return {
        "project": "Проект",
        "norm": "Нормативы",
        "estimate": "Сметы",
        "catalog": "Каталог",
        "cad_bim": "CAD/BIM",
    }.get(kind, "Данные")


def build_samovar(
    *,
    can_manage: bool = True,
    open_tab: str = "data",
    workspace_title: str = "Данные",
):
    """Data catalog with role-gated index and registration controls."""
    _S = {
        "rows": [],
        "q": "",
        "filter": "all",
        "jobs": [],
        "memory": {},
        "readiness": {},
        "index_settings": dict(_DEFAULT_INDEX_SETTINGS),
    }
    _refs = {"disp": None, "status": None, "ops": None}

    def _notify(message: str, type: str = "info", **kwargs):
        try:
            ui.notify(message, type=type, **kwargs)
        except RuntimeError as err:
            add_log(f"[UI] уведомление не показано: {err}")
        except Exception:
            pass

    async def _pick_local_folder(*, initial: str = "", title: str = "Выберите папку") -> str:
        query = urlencode({"initial": initial or "", "title": title})
        data = await api_get(f"/lite-runtime/pick-folder?{query}", base=_LOCAL_UI_BASE)
        if isinstance(data, dict) and data.get("status") == "selected" and data.get("path"):
            return str(data["path"])
        if isinstance(data, dict) and data.get("status") == "cancelled":
            return ""
        detail = last_api_error_text("Локальный выбор папки недоступен")
        _notify(f"{detail}. Используй Обзор…", type="warning")
        return ""

    def _settings_changed() -> bool:
        cur = _S.get("index_settings") or {}
        return any(cur.get(k) != v for k, v in _DEFAULT_INDEX_SETTINGS.items())

    def _setting(key: str):
        return (_S.get("index_settings") or {}).get(key, _DEFAULT_INDEX_SETTINGS[key])

    def _set_setting(key: str, value):
        settings = _S.setdefault("index_settings", dict(_DEFAULT_INDEX_SETTINGS))
        if isinstance(_DEFAULT_INDEX_SETTINGS[key], bool):
            settings[key] = bool(value)
        elif isinstance(_DEFAULT_INDEX_SETTINGS[key], int):
            settings[key] = int(value or _DEFAULT_INDEX_SETTINGS[key])
        else:
            settings[key] = float(value or _DEFAULT_INDEX_SETTINGS[key])
        _render_ops()

    def _reset_index_settings():
        _S["index_settings"] = dict(_DEFAULT_INDEX_SETTINGS)
        for key, ref in (_refs.get("settings") or {}).items():
            if key in _DEFAULT_INDEX_SETTINGS:
                ref.value = _DEFAULT_INDEX_SETTINGS[key]
                ref.update()
        _render_ops()
        _notify("Настройки индексации сброшены к умолчанию", type="info")

    def _scheduler_payload() -> dict:
        return {
            "batch_limit": int(_setting("batch_limit")),
            "max_batches": int(_setting("max_batches")),
            "cooldown_sec": float(_setting("cooldown_sec")),
            "min_free_gb": float(_setting("min_free_gb")),
            "max_swap_pct": float(_setting("max_swap_pct")),
            "unload_between_batches": bool(_setting("unload_between_batches")),
            "unload_before_start": bool(_setting("unload_before_start")),
            "background": True,
        }

    def _queue_counts() -> dict[str, int]:
        rows = _S.get("rows") or []
        return {
            "pending": sum(int(r.get("pending") or 0) for r in rows),
            "light": sum(int(r.get("pending_light") or 0) for r in rows),
            "ocr": sum(int(r.get("pending_ocr") or 0) for r in rows),
            "unknown": sum(int(r.get("pending_unknown") or 0) for r in rows),
            "errors": sum(int(r.get("error") or 0) for r in rows),
        }

    def _job_status_color(status: str) -> str:
        s = str(status or "").upper()
        if s in {"QUEUED", "RUNNING", "PARSING", "STARTED"}:
            return "var(--warn)"
        if s in {"COMPLETED", "PARTIAL"}:
            return "var(--ok)"
        if s in {"FAILED", "ERROR", "CANCELLED"}:
            return "var(--err)"
        return "var(--dim)"

    def _job_status_label(status: str) -> str:
        return {
            "QUEUED": "В ОЧЕРЕДИ",
            "RUNNING": "ВЫПОЛНЯЕТСЯ",
            "PARSING": "РАЗБОР",
            "STARTED": "ЗАПУЩЕНО",
            "COMPLETED": "ГОТОВО",
            "PARTIAL": "ЧАСТИЧНО",
            "FAILED": "ОШИБКА",
            "ERROR": "ОШИБКА",
            "CANCELLED": "ОТМЕНЕНО",
        }.get(str(status or "").upper(), str(status or "—").upper())

    def _render_ops():
        panel = _refs.get("ops")
        if panel is None:
            return
        panel.clear()
        counts = _queue_counts()
        memory_state = _S.get("memory") or {}
        mem = memory_state.get("memory") if isinstance(memory_state.get("memory"), dict) else {}
        reason = str(memory_state.get("reason") or "")
        state_name = str(memory_state.get("state") or "UNKNOWN")
        jobs = _S.get("jobs") or []
        active = [
            j for j in jobs
            if str(j.get("status", "")).upper() in {"QUEUED", "RUNNING", "PARSING", "STARTED"}
            and "parse" in str(j.get("type", "")).lower()
        ]
        recent = [j for j in jobs if "parse" in str(j.get("type", "")).lower()][:5]
        with panel:
            with ui.element("div").classes("sov-dataset-operator-summary"):
                with ui.row().classes("items-center w-full sov-dataset-operator-line"):
                    ui.icon("o_radar").classes("sov-dataset-operator-icon")
                    ui.label("Оператор индекса · состояние очереди").classes("sov-dataset-operator-title")
                    status_badge(
                        f"{counts['pending']} ждут" if counts["pending"] else "очередь пуста",
                        "warn" if counts["pending"] else "ok",
                    )
                    ui.label(f"лёгкие файлы {counts['light']}").classes("sov-dataset-operator-fact")
                    ui.label(f"сканы {counts['ocr']}").classes("sov-dataset-operator-fact")
                    if counts["unknown"]:
                        ui.label(f"не распознано {counts['unknown']}").classes("sov-dataset-operator-fact")
                    if counts["errors"]:
                        status_badge(f"ошибки {counts['errors']}", "error")
                    ui.element("div").classes("sov-flex-spacer")
                    mem_color = "var(--ok)" if state_name in {"GREEN", "OK"} else "var(--warn)" if state_name in {"YELLOW", "RED"} else "var(--err)"
                    memory_label = {"GREEN": "НОРМА", "OK": "НОРМА", "YELLOW": "МАЛО", "RED": "КРИТИЧНО"}.get(
                        state_name, "НЕТ ДАННЫХ"
                    )
                    ui.label(
                        f"ОЗУ свободно {float(mem.get('ram_free_gb') or 0):.1f} ГБ · {memory_label}"
                    ).classes("sov-dataset-operator-memory").style(f"color:{mem_color};")
                if reason:
                    swap_pct = float(mem.get("swap_pct") or 0)
                    ui.label(
                        "Памяти достаточно для разбора файлов."
                        if state_name in {"GREEN", "OK"}
                        else f"Разбор ограничен по памяти; файл подкачки занят на {swap_pct:.0f}%."
                    ).classes("sov-dataset-operator-note")
                if counts["ocr"] and state_name in {"RED", "CRITICAL"}:
                    ui.label(
                        "Сканы лучше отложить: можно разбирать текст, документы и таблицы, а тяжёлые сканы оставить до нормальной памяти."
                    ).classes("sov-dataset-operator-note sov-dataset-operator-note--warn")
                if _settings_changed():
                    ui.label(
                        "Настройки отличаются от умолчания. Меняйте их только для конкретной задачи."
                    ).classes("sov-dataset-operator-note sov-dataset-operator-note--warn")
                if active:
                    for job in active[:3]:
                        total = int(job.get("total") or 0)
                        processed = int(job.get("processed") or 0)
                        pct = float(job.get("percent") or 0)
                        eta = str(job.get("eta_text") or "")
                        msg = str(job.get("message") or "")
                        with ui.column().classes("w-full sov-dataset-active-job"):
                            with ui.row().classes("items-center w-full sov-dataset-active-job__head"):
                                ui.label(
                                    str(job.get("dataset_name") or job.get("source") or "очередь")
                                ).classes("sov-dataset-active-job__title")
                                ui.label(str(job.get("id") or "")[:12]).classes("sov-dataset-active-job__id")
                                ui.label(_job_status_label(str(job.get("status") or ""))).classes(
                                    "sov-dataset-active-job__status"
                                ).style(f"color:{_job_status_color(str(job.get('status') or ''))};")
                                if eta:
                                    ui.label(f"Осталось {eta}").classes("sov-dataset-active-job__meta")
                            ui.linear_progress(value=max(0.0, min(1.0, pct / 100.0))).props(
                                "instant-feedback color=green"
                            ).classes("sov-dataset-active-job__progress")
                            ui.label(f"{processed}/{total} · {msg}").classes("sov-dataset-active-job__meta")
                elif recent:
                    last = recent[0]
                    last_status = str(last.get("status") or "").upper()
                    last_message = str(last.get("message") or last.get("id") or "")
                    readiness = _S.get("readiness") or {}
                    general = readiness.get("general") if isinstance(readiness.get("general"), dict) else {}
                    notice = _operator_queue_notice(
                        pending=counts["pending"],
                        last_status=last_status,
                        last_message=last_message,
                        contract_compatible=bool(general.get("contract_compatible")),
                    )
                    if notice:
                        message, tone = notice
                        ui.label(message).classes(
                            f"sov-dataset-operator-notice sov-dataset-operator-notice--{tone}"
                        )
                    else:
                        ui.label(
                            f"Последняя задача: {_job_status_label(last_status)} · {last_message}"
                        ).classes("sov-dataset-operator-note")

    def _ui_handler(coro_func, *args, **kwargs):
        async def _handler(*_event_args):
            await coro_func(*args, **kwargs)

        return _handler

    def _grid_handler(coro_func):
        async def _handler(event):
            await coro_func(event.args)

        return _handler

    def _error_hint(err: str) -> str:
        e = (err or "").lower()
        if any(k in e for k in ("memory", "память", "swap", "oom")):
            return "Память: уменьшить batch/cooldown, выгрузить MLX, затем Ремонт"
        if any(k in e for k in ("timeout", "таймаут", "1800")):
            return "Завис/большой: меньший лимит парсинга, затем Ремонт"
        if any(k in e for k in ("not found", "не найден", "no such file")):
            return "Файл переехал/удалён: пересинк папки-источника"
        if any(k in e for k in ("corrupt", "поврежд", "no stream")):
            return "Файл повреждён: пересоздать или пропустить"
        if any(k in e for k in ("unsupported", "не поддерж")):
            return "Формат не поддержан: конвертировать в pdf/docx"
        return "Открой список файлов — точный текст ошибки"

    def _agg(docs):
        by = {}
        for d in (docs or {}).get("documents", []):
            s = by.setdefault(d.get("dataset_id"), {
                "INDEXED": 0,
                "PENDING": 0,
                "ERROR": 0,
                "MISSING": 0,
                "chunks": 0,
                "without_chunks": 0,
                "pending_ocr": 0,
                "pending_light": 0,
                "pending_unknown": 0,
            })
            st = d.get("status", "")
            if st in s and st != "chunks":
                s[st] += 1
            s["chunks"] += int(d.get("chunk_count") or 0)
            if st == "INDEXED" and not int(d.get("chunk_count") or 0):
                s["without_chunks"] += 1
            if st == "PENDING":
                complexity = str(d.get("complexity") or "").lower()
                pipeline = str(d.get("pipeline") or "").lower()
                if complexity == "needs_ocr" or pipeline == "markdown_needs_ocr":
                    s["pending_ocr"] += 1
                elif complexity or pipeline:
                    s["pending_light"] += 1
                else:
                    s["pending_unknown"] += 1
        return by

    async def _load():
        ds = await api_get("/api/rag/datasets") or []
        # Эндпоинт документов капит лимит на 500 (le=500): limit>500 → 422 → пусто (баг v1).
        # Пагинируем по 500, агрегируем все.
        all_docs = []
        offset = 0
        while True:
            page = await api_get(f"/api/rag/documents?limit=500&offset={offset}")
            items = (page or {}).get("documents", []) if isinstance(page, dict) else []
            all_docs.extend(items)
            if len(items) < 500 or offset >= 30000:
                break
            offset += 500
        agg = _agg({"documents": all_docs})
        rows = []
        for d in (ds if isinstance(ds, list) else ds.get("datasets", []) or []):
            did = d.get("id") or d.get("dataset_id")
            a = agg.get(did, {
                "INDEXED": 0,
                "PENDING": 0,
                "ERROR": 0,
                "MISSING": 0,
                "chunks": 0,
                "without_chunks": 0,
                "pending_ocr": 0,
                "pending_light": 0,
                "pending_unknown": 0,
            })
            tot = a["INDEXED"] + a["PENDING"] + a["ERROR"] + a["MISSING"]
            rows.append({"id": did, "name": d.get("name", "?"), "sensitivity": d.get("sensitivity", "P0"),
                         "group": d.get("group_name", ""), "indexed": a["INDEXED"], "pending": a["PENDING"],
                         "pending_ocr": a["pending_ocr"], "pending_light": a["pending_light"],
                         "pending_unknown": a["pending_unknown"], "error": a["ERROR"], "missing": a["MISSING"],
                         "chunks": a["chunks"], "total": tot,
                         "without_chunks": a["without_chunks"], "ready_files": 0,
                         "search_ready": False, "readiness": {},
                         "dataset_kind": d.get("dataset_kind") or d.get("kind") or "",
                         "source_type": d.get("source_type") or "",
                         "dataset_scope": d.get("dataset_scope") or "user"})
        # A processing flag is not proof that the native search index exists.
        # Bound parallel checks so a large catalog cannot flood the local API.
        gate = asyncio.Semaphore(4)
        async def check_index(row):
            if not row["chunks"] or not row["indexed"]:
                return
            async with gate:
                try:
                    result = await asyncio.wait_for(api_get(
                        "/api/rag/readiness?" + urlencode({"dataset_id": row["id"]})
                    ), timeout=8)
                except Exception:
                    result = None
            general = result.get("general", {}) if isinstance(result, dict) else {}
            row["readiness"] = general
            row["search_ready"] = bool(
                result and result.get("status") == "ok" and general.get("rrf_ready")
                and general.get("activated") and general.get("points") == row["chunks"]
            )
            if row["search_ready"]:
                row["ready_files"] = row["indexed"] - row["without_chunks"]
        await asyncio.gather(*(check_index(row) for row in rows))
        rows.sort(key=lambda r: (0 if r["error"] else 1, 0 if r["pending"] else 1, r["name"].lower()))
        _S["rows"] = rows

    def _light(r):
        if r["error"]:
            return ("var(--err)", f"{r['error']} ошибок", "o_error")
        if r.get("missing"):
            return ("var(--err)", f"{r['missing']} источников пропало", "o_link_off")
        if r["pending"]:
            return ("var(--warn)", f"Ждёт {r['pending']} · готово {r['ready_files']}/{r['total']}", "o_schedule")
        if r["without_chunks"]:
            return ("var(--warn)", f"Нет фрагментов у {r['without_chunks']} файлов", "o_warning_amber")
        if r["indexed"]:
            if r["search_ready"]:
                return ("var(--ok)", "Готов к поиску", "o_check_circle")
            return ("var(--warn)", "Поисковый индекс не подтверждён", "o_warning_amber")
        return ("var(--dim)", "Пусто", "o_remove")

    def _bar(r):
        with ui.element("div").classes("sov-dataset-progress"):
            for n, col in ((r["ready_files"], "var(--ok)"),
                           (r["pending"] + r["indexed"] - r["ready_files"], "var(--warn)"),
                           (r["error"] + r["missing"], "var(--err)")):
                if n:
                    ui.element("div").style(f"flex:{n};background:{col};")

    async def _parse(r):
        # «Плей» = background job на одну партию. GUI верит jobs API, а не оптимистичной кнопке.
        nm = r.get("name", "?")
        limit = int(_setting("row_batch_limit"))
        add_log(f"[ПАРС] ▶ {nm}: партия до {limit} файлов…")
        _notify(f"▶ Парсинг «{nm}» — ставлю job на партию до {limit} файлов…", type="info")
        try:
            d = await api_post(f"/api/rag/parse-batch/{r['id']}?{urlencode({'limit': limit, 'background': 'true'})}", {})
        except Exception as e:  # noqa: BLE001
            add_log(f"[ПАРС] ✗ {nm}: {e}")
            _notify(last_api_error_text(f"Парсинг «{nm}» не запустился"), type="negative")
            return
        if not d:
            add_log(f"[ПАРС] ✗ {nm}: отказ (вероятно, защита памяти — см. статус)")
            _notify(last_api_error_text(f"Парсинг «{nm}»: отказ (память?)"), type="negative")
            await _refresh_status()
            return
        msg = f"✓ «{nm}»: job {d.get('job_id', '?')} создана · pending {d.get('pending', 0)}"
        add_log(f"[ПАРС] {msg}")
        _notify(msg, type="positive")
        await _refresh_status()

    async def _repair(r):
        d = await api_post(f"/api/rag/datasets/{r['id']}/repair", {})
        n = (d or {}).get("requeued", 0)
        damaged = (d or {}).get("encoding_requeued", 0)
        job_id = (d or {}).get("job_id") or "?"
        _notify((f"Ремонт «{r['name']}» запущен: файлов {n}, повреждённый текст {damaged} · задача {job_id}")
                if n else "Повреждений не найдено",
                  type="warning" if n else "info")
        await _refresh()

    async def _delete(r):
        from sovushka.components.dataset_delete import open_dataset_delete
        open_dataset_delete(r, on_deleted=_refresh)

    async def _ask_project(r):
        ds_id = str((r or {}).get("id") or (r or {}).get("dataset_id") or "").strip()
        name = str((r or {}).get("name") or (r or {}).get("folder") or ds_id).strip()
        if not ds_id:
            ui.notify("У датасета нет id", type="warning")
            return
        question = (
            f"прочитай проектный датасет «{name}»: что это за проект, "
            "какие тома/разделы/таблицы/чертежи видны, где искать ключевые данные и что не прочитано"
        )
        params = urlencode({"scope": f"ds:{ds_id}", "question": question, "tab": "chat"})
        path = str(getattr(context.client.request, "url", "") or "")
        target_path = "/les/classic" if "/les/classic" in path else "/classic"
        ui.navigate.to(f"{target_path}?{params}")

    async def _start_all():
        payload = _scheduler_payload()
        add_log("[PARSE_SCHEDULER] top Пуск → /api/rag/parse-scheduler")
        d = await api_post("/api/rag/parse-scheduler", payload)
        if d:
            _notify(f"Индексатор запущен: job {d.get('job_id', '?')}", type="positive")
            add_log(f"[PARSE_SCHEDULER] job {d.get('job_id', '?')} queued")
        else:
            _notify(last_api_error_text("Индексатор не запустился"), type="negative")
        await _refresh_status()

    async def _stop_all():
        await api_post("/api/runtime/dispatcher/reindex/pause", {"reason": "operator"})
        _notify("Индексатор остановлен", type="warning")
        await _refresh_status()

    async def _open_files(r):
        ds_id = str((r or {}).get("id") or (r or {}).get("dataset_id") or "").strip()
        if not ds_id:
            _notify("У датасета нет id", type="warning")
            return
        request_path = str(getattr(context.client.request, "url", "") or "")
        target_path = "/les/classic" if "/les/classic" in request_path else "/classic"
        ui.navigate.to(f"{target_path}?{urlencode({'tab': open_tab, 'dataset_id': ds_id})}")

    registry_dialog = ui.dialog() if can_manage else None

    async def _open_registry(r):
        if registry_dialog is None or not isinstance(r, dict):
            return
        ds_id = str((r or {}).get("id") or (r or {}).get("dataset_id") or "").strip()
        ds_name = str((r or {}).get("name") or (r or {}).get("folder") or "Датасет")

        registry_dialog.clear()
        with registry_dialog, ui.card().classes("sov-advanced-dialog").style("width:min(1180px,96vw);max-width:96vw;max-height:90vh;"):
            with ui.row().classes("items-center justify-between w-full gap-3"):
                with ui.column().classes("gap-0"):
                    ui.label(f"РЕЕСТР ФАЙЛОВ // {ds_name}").classes("sov-panel-title")
                    ui.label(f"dataset_id: {ds_id} · всего файлов: {r.get('total', 0)}").classes("sov-muted")
                ui.button(icon="o_close", on_click=registry_dialog.close).props("flat round dense")

            with ui.row().classes("w-full gap-3"):
                reg_kpi = {}
                for k, lbl, col in [
                    ("total", "Файлов", "var(--text)"),
                    ("indexed", "В индексе", "var(--ok)"),
                    ("pending", "Ожидают", "var(--warn)"),
                    ("errors", "Ошибки", "var(--err)"),
                    ("chunks", "Чанков", "var(--text)"),
                ]:
                    with ui.card().classes("kpi-box flex-1"):
                        reg_kpi[k] = ui.label("—").classes("kpi-val").style(f"color:{col};font-size:1.35rem;font-weight:900;")
                        ui.label(lbl).classes("kpi-lbl").style("font-size:.6rem;text-transform:uppercase;color:var(--dim);margin-top:4px;")

            with ui.row().classes("items-center gap-2 w-full"):
                status_sel = ui.select(
                    {"": "Все статусы", "INDEXED": "INDEXED", "PENDING": "PENDING", "ERROR": "ERROR", "MISSING": "MISSING"},
                    value="",
                    label="Статус",
                ).props("dense outlined emit-value map-options").style("width:150px;font-size:.7rem;")
                q_in = ui.input(placeholder="Поиск по имени файла, ошибке...").props("dense outlined clearable").classes("flex-1").style("font-size:.7rem;")
                limit_sel = ui.select([50, 100, 250, 500], value=100, label="Лимит").props("dense outlined").style("width:90px;font-size:.7rem;")

                async def _load_docs():
                    params = {"dataset_id": ds_id, "limit": limit_sel.value or 100}
                    if status_sel.value:
                        params["status"] = status_sel.value
                    if (q_in.value or "").strip():
                        params["q"] = (q_in.value or "").strip()
                    res = await api_get(f"/api/rag/documents?{urlencode(params)}")
                    if not isinstance(res, dict):
                        _notify(last_api_error_text("Ошибка загрузки файлов из реестра"), type="negative")
                        return
                    docs = res.get("documents", [])
                    summary = res.get("summary", {}) if isinstance(res.get("summary"), dict) else {}
                    idx_cnt = summary.get("INDEXED", {}).get("files", 0)
                    pnd_cnt = summary.get("PENDING", {}).get("files", 0)
                    err_cnt = summary.get("ERROR", {}).get("files", 0)
                    tot_cnt = res.get("total", len(docs))
                    chunks_cnt = sum(int(v.get("chunks") or 0) for v in summary.values() if isinstance(v, dict))

                    reg_kpi["total"].set_text(str(tot_cnt))
                    reg_kpi["indexed"].set_text(str(idx_cnt))
                    reg_kpi["pending"].set_text(str(pnd_cnt))
                    reg_kpi["errors"].set_text(str(err_cnt))
                    reg_kpi["chunks"].set_text(str(chunks_cnt))

                    doc_rows = []
                    for doc in docs:
                        if not isinstance(doc, dict):
                            continue
                        doc_rows.append({
                            "id": doc.get("id", ""),
                            "status": doc.get("status", "PENDING"),
                            "file": doc.get("file_name") or doc.get("name") or "—",
                            "chunks": doc.get("chunk_count", 0),
                            "size": f"{(doc.get('file_size') or 0)/1024:.1f} KB" if doc.get("file_size") else "—",
                            "source": doc.get("source_path") or doc.get("path") or "",
                            "error": doc.get("last_error") or "",
                        })
                    doc_grid.rows = doc_rows
                    doc_grid.update()

                ui.button(icon="o_search", on_click=lambda: asyncio.create_task(_load_docs())).props("flat round dense").tooltip("Поиск")
                ui.button(icon="o_done_all", on_click=lambda: (setattr(status_sel, "value", "INDEXED"), asyncio.create_task(_load_docs()))).props("flat round dense").tooltip("Только INDEXED")
                ui.button(icon="o_pending_actions", on_click=lambda: (setattr(status_sel, "value", "PENDING"), asyncio.create_task(_load_docs()))).props("flat round dense").tooltip("Только PENDING")
                ui.button(icon="o_error_outline", on_click=lambda: (setattr(status_sel, "value", "ERROR"), asyncio.create_task(_load_docs()))).props("flat round dense").tooltip("Только ERROR")

            cols = [
                {"name": "status", "label": "Статус", "field": "status", "align": "left", "sortable": True},
                {"name": "file", "label": "Файл", "field": "file", "align": "left", "sortable": True},
                {"name": "chunks", "label": "Чанков", "field": "chunks", "align": "center", "sortable": True},
                {"name": "size", "label": "Размер", "field": "size", "align": "right", "sortable": True},
                {"name": "source", "label": "Путь к файлу", "field": "source", "align": "left"},
                {"name": "error", "label": "Ошибка", "field": "error", "align": "left"},
            ]
            doc_grid = ui.table(columns=cols, rows=[], row_key="id", pagination=20).classes("w-full").props("dense wrap-cells").style(
                "background:var(--bg-panel);color:var(--text);font-family:var(--font);"
            )
            doc_grid.add_slot("body-cell-status", """
                <q-td :props="props">
                  <span :style="{color: props.value === 'INDEXED' ? '#10b981' : props.value === 'ERROR' ? '#ef4444' : '#f59e0b', fontWeight:'900'}">
                    {{ props.value }}
                  </span>
                </q-td>""")
            doc_grid.add_slot("body-cell-file", """
                <q-td :props="props">
                  <div :title="props.value" style="max-width:380px;white-space:normal;word-break:break-word;font-family:var(--font-chat);font-size:.68rem;font-weight:700;">
                    {{ props.value }}
                  </div>
                </q-td>""")
            doc_grid.add_slot("body-cell-source", """
                <q-td :props="props">
                  <span v-if="props.value" :title="props.value" style="color:var(--dim);white-space:normal;word-break:break-all;font-size:.62rem;font-family:var(--font-chat);">
                    {{ props.value }}
                  </span>
                  <span v-else style="color:var(--dim);">—</span>
                </q-td>""")
            doc_grid.add_slot("body-cell-error", """
                <q-td :props="props">
                  <span v-if="props.value" :title="props.value" style="color:#ef4444;white-space:normal;word-break:break-word;font-size:.66rem;">
                    {{ props.value }}
                  </span>
                  <span v-else style="color:var(--dim);">—</span>
                </q-td>""")

            status_sel.on("update:model-value", lambda e: asyncio.create_task(_load_docs()))
            q_in.on("keydown.enter", lambda e: asyncio.create_task(_load_docs()))
            limit_sel.on("update:model-value", lambda e: asyncio.create_task(_load_docs()))

            registry_dialog.open()
            await _load_docs()

    def _rename_dataset(r):
        dlg = ui.dialog()
        with dlg, panel(variant="raised", classes="p-4 rounded-xl min-w-[360px]"):
            ui.label(f"Переименование «{r['name']}»").classes("text-subtitle1 font-bold mb-2")
            inp = text_field(label="Новое название датасета", value=r["name"], classes="w-full")
            async def _save():
                val = (inp.value or "").strip()
                if not val:
                    ui.notify("Название не может быть пустым", type="warning")
                    return
                res = await api_patch(f"/api/rag/datasets/{quote(r['id'], safe='')}/name?name={quote(val, safe='')}")
                if res and res.get("name") == val:
                    dlg.close()
                    ui.notify(f"Датасет переименован в «{val}»", type="positive")
                    await _refresh()
                else:
                    ui.notify("Не удалось переименовать датасет", type="negative")
            with ui.row().classes("justify-end w-full mt-4 gap-2"):
                action_button("Отмена", on_click=dlg.close, variant="quiet", compact=True)
                action_button("Сохранить", icon="o_save", on_click=_save, variant="primary", compact=True)
        dlg.open()

    def _change_group(r):
        dlg = ui.dialog()
        with dlg, panel(variant="raised", classes="p-4 rounded-xl min-w-[360px]"):
            ui.label(f"Группа датасета «{r['name']}»").classes("text-subtitle1 font-bold mb-2")
            ui.label("Группируйте датасеты (например: Почта, Рабочая документация)").classes("sov-muted mb-2")
            inp = text_field(label="Название группы", value=r.get("group") or "", placeholder="Например: Почта", classes="w-full")
            async def _save():
                val = (inp.value or "").strip()
                res = await api_patch(f"/api/rag/datasets/{quote(r['id'], safe='')}/group?group={quote(val, safe='')}")
                if res is not None:
                    dlg.close()
                    msg = f"Группа установлена: «{val}»" if val else "Группа сброшена"
                    ui.notify(msg, type="positive")
                    await _refresh()
                else:
                    ui.notify("Не удалось обновить группу", type="negative")
            with ui.row().classes("justify-end w-full mt-4 gap-2"):
                action_button("Отмена", on_click=dlg.close, variant="quiet", compact=True)
                action_button("Сохранить", icon="o_save", on_click=_save, variant="primary", compact=True)
        dlg.open()

    add_dialog = ui.dialog() if can_manage else None

    def _open_add():
        open_folder_setup(add_dialog, pick_folder=_pick_local_folder, on_done=_refresh)

    def _row_actions(r):
        action_button(
            "Открыть файлы",
            icon="o_folder_open",
            on_click=_ui_handler(_open_files, r),
            variant="primary",
            compact=True,
        )
        if can_manage:
            with action_button(
                icon="o_more_horiz",
                variant="quiet",
                compact=True,
                icon_only=True,
                aria_label=f"Другие действия: {r['name']}",
                classes="sov-dataset-more",
            ):
                with ui.menu().classes("sov-dataset-actions-menu"):
                    ui.menu_item("Диагностика файлов", on_click=_ui_handler(_open_registry, r))
                    ui.menu_item("Переименовать датасет", on_click=_ui_handler(_rename_dataset, r))
                    ui.menu_item("Изменить группу", on_click=_ui_handler(_change_group, r))
                    ui.menu_item("О проекте", on_click=_ui_handler(_ask_project, r))
                    ui.menu_item("Продолжить обработку", on_click=_ui_handler(_parse, r))
                    ui.menu_item("Проверить и починить", on_click=_ui_handler(_repair, r))
                    ui.separator()
                    ui.menu_item("Удалить датасет", on_click=_ui_handler(_delete, r)).classes(
                        "sov-dataset-menu-danger"
                    )

    def _visible_rows():
        q = (_S.get("q") or "").strip().lower()
        f = _S.get("filter", "all")
        out = []
        for r in _S["rows"]:
            if q and q not in str(r["name"]).lower():
                continue
            if f == "indexed" and not (r["total"] > 0 and r["ready_files"] == r["total"]):
                continue
            if f == "pending" and r["pending"] <= 0:
                continue
            if f == "error" and r["error"] <= 0:
                continue
            if f == "empty" and r["total"] > 0:
                continue
            out.append(r)
        sk = _S.get("sort")
        if sk:
            keyf = {"files": lambda r: r["total"], "chunks": lambda r: r["chunks"],
                    "name": lambda r: str(r["name"]).lower()}.get(sk)
            if keyf:
                out.sort(key=keyf, reverse=_S.get("sort_dir", -1) < 0)
        return out

    def _render_rows():
        disp = _refs["disp"]
        if disp is None:
            return
        disp.clear()
        with disp:
            if not _S["rows"]:
                render_feedback_state(
                    "empty",
                    detail="Добавьте папку с документами — ЛЕС зарегистрирует её как отдельный датасет.",
                )
                return
            vis = _visible_rows()
            if not vis:
                render_feedback_state(
                    "empty",
                    detail="Поиск и выбранный фильтр не совпали ни с одним датасетом.",
                )
                return
            with ui.column().classes("w-full sov-dataset-registry"):
                for r in vis:
                    col, txt, ico = _light(r)
                    tone = (
                        "error"
                        if r["error"] or r.get("missing")
                        else "warn"
                        if r["pending"] or r["without_chunks"] or (r["indexed"] and not r["search_ready"])
                        else "ok"
                        if r["ready_files"]
                        else "muted"
                    )
                    with ui.element("article").classes("sov-dataset-row"):
                        with ui.row().classes("items-center w-full sov-dataset-row__head"):
                            with ui.element("div").classes("sov-dataset-row__identity"):
                                ui.icon(ico).classes("sov-dataset-row__state-icon").style(f"color:{col};")
                                with ui.column().classes("sov-dataset-row__copy"):
                                    ui.label(r["name"]).classes("sov-dataset-row__name")
                                    scope = _dataset_source_label(r)
                                    group = f" · {r['group']}" if r.get("group") else ""
                                    ui.label(f"{scope}{group}").classes("sov-dataset-row__scope")
                            status_badge(txt, tone)
                        with ui.element("div").classes("sov-dataset-row__facts"):
                            with ui.element("div").classes("sov-dataset-fact"):
                                ui.label("Файлы").classes("sov-dataset-fact__label")
                                ui.label(str(r["total"])).classes("sov-dataset-fact__value")
                            with ui.element("div").classes("sov-dataset-fact"):
                                ui.label("Готовы к поиску").classes("sov-dataset-fact__label")
                                ui.label(str(r["ready_files"])).classes("sov-dataset-fact__value")
                            with ui.element("div").classes("sov-dataset-fact"):
                                ui.label("Ждут").classes("sov-dataset-fact__label")
                                ui.label(str(r["pending"])).classes("sov-dataset-fact__value")
                            with ui.element("div").classes("sov-dataset-fact"):
                                ui.label("Ошибки").classes("sov-dataset-fact__label")
                                ui.label(str(r["error"])).classes("sov-dataset-fact__value")
                            with ui.element("div").classes("sov-dataset-fact"):
                                ui.label("Фрагменты").classes("sov-dataset-fact__label")
                                ui.label(str(r["chunks"])).classes("sov-dataset-fact__value")
                        with ui.element("div").classes("sov-dataset-row__progress"):
                            _bar(r)
                        if r["pending"]:
                            detail = (
                                f"В очереди: текст и таблицы {r.get('pending_light', 0)} · "
                                f"сканы {r.get('pending_ocr', 0)}"
                            )
                            if r.get("pending_unknown"):
                                detail += f" · тип не определён {r.get('pending_unknown', 0)}"
                            ui.label(detail).classes("sov-dataset-row__note")
                        with ui.row().classes("items-center w-full sov-dataset-row__actions"):
                            _row_actions(r)

    async def _refresh_status():
        if not can_manage:
            return
        # Тикает каждые 5с НЕЗАВИСИМО от _parse. Верим активной job, а не stale dataset.status:
        # PENDING — это очередь, PARSING — только если есть живой scheduler/batch.
        st = await api_get("/api/runtime/dispatcher/reindex/status") or {}
        idx = await api_get("/api/indexing-mode") or {}
        disp = bool(st.get("running"))
        jobs = await api_get("/api/jobs/summary?limit=40") or {}
        readiness = await api_get("/api/rag/readiness") or {}
        job_items = jobs.get("jobs", []) if isinstance(jobs, dict) else []
        _S["jobs"] = job_items
        _S["readiness"] = readiness if isinstance(readiness, dict) else {}
        if isinstance(idx, dict):
            _S["memory"] = idx.get("memory_state") if isinstance(idx.get("memory_state"), dict) else {}
        active_parse = [
            j for j in job_items
            if str(j.get("status", "")).upper() in {"QUEUED", "RUNNING", "PARSING", "STARTED"}
            and (j.get("type") == "rag_parse_scheduler" or "parse" in str(j.get("type", "")).lower())
        ]
        pending = sum(int(r.get("pending") or 0) for r in _S.get("rows", []))
        if _refs["status"]:
            if active_parse:
                current = active_parse[0]
                _refs["status"].set_text(
                    f"Индексатор: выполняется задача {str(current.get('id', ''))[:12]} "
                    f"{current.get('processed', 0)}/{current.get('total', 0)}"
                )
            elif disp:
                _refs["status"].set_text("Индексатор включён, но активной задачи разбора нет")
            elif pending:
                _refs["status"].set_text(f"Индексатор: простаивает · ждут {pending} файлов")
            else:
                _refs["status"].set_text("Индексатор: простаивает")
        _render_ops()

    async def _refresh():
        try:
            await _load()
            if _refs.get("stats"):
                rows = _S["rows"]
                vals = {"datasets": len(rows), "files": sum(r["total"] for r in rows),
                        "indexed": sum(r["ready_files"] for r in rows), "pending": sum(r["pending"] for r in rows),
                        "error": sum(r["error"] for r in rows), "chunks": sum(r["chunks"] for r in rows)}
                for k, lbl in _refs["stats"].items():
                    lbl.set_text(f"{vals.get(k, 0):,}".replace(",", " "))
            _render_rows()
            await _refresh_status()
        except Exception as exc:  # noqa: BLE001 — рендер не должен ронять страницу
            if _refs["disp"]:
                _refs["disp"].clear()
                with _refs["disp"]:
                    ui.label(f"Ошибка загрузки датасетов: {exc}").style("color:var(--err);padding:16px;")

    def _set_filter(m):
        _S["filter"] = m
        for key, btn in (_refs.get("fbtn") or {}).items():
            btn.classes(remove="sov-dataset-filter--active")
            if key == m:
                btn.classes(add="sov-dataset-filter--active")
        _render_rows()

    with ui.column().classes("w-full sov-datasets-page"):
        with panel(variant="raised", classes="sov-datasets-hero"):
            ui.label("ВАШИ ИСТОЧНИКИ").classes("sov-workspace-eyebrow")
            with ui.row().classes("items-center w-full sov-datasets-hero__row"):
                ui.icon("o_folder_open").classes("sov-datasets-hero__icon")
                ui.label(workspace_title).classes("sov-datasets-hero__title")
                ui.element("div").classes("sov-flex-spacer")
                if can_manage:
                    action_button(
                        "Добавить набор",
                        icon="o_add",
                        on_click=_open_add,
                        variant="primary",
                        classes="sov-dataset-add",
                    )
            ui.label(
                "Ваши документы для ответов с источниками. "
                "Подключите папку, следите за изменениями и открывайте нужные файлы."
            ).classes("sov-datasets-hero__detail")

        _refs["stats"] = {}
        with panel(variant="inset", classes="sov-dataset-summary"):
            with ui.element("div").classes("sov-dataset-summary__copy"):
                ui.label("Готовность документов").classes("sov-dataset-summary__title")
                ui.label("Только фактические файлы и состояние индекса").classes(
                    "sov-dataset-summary__detail"
                )
            with ui.element("div").classes("sov-dataset-summary__metrics"):
                for _k, _lbl in (
                    ("datasets", "наборов"),
                    ("files", "файлов"),
                    ("indexed", "готово к поиску"),
                    ("pending", "ожидают обработки"),
                    ("error", "требуют внимания"),
                    ("chunks", "фрагментов"),
                ):
                    with ui.element("div").classes("sov-dataset-summary__metric"):
                        _refs["stats"][_k] = ui.label("—").classes("sov-dataset-summary__value")
                        ui.label(_lbl).classes("sov-dataset-summary__label")

        with ui.expansion(
            "Обработка документов",
            icon="o_settings_input_component",
            value=False,
        ).classes("w-full sov-dataset-disclosure") as operator_disclosure:
            ui.label(
                "Служебные операции и настройки индексации. "
                "Для просмотра и поиска по готовым данным открывать этот блок не нужно."
            ).classes("sov-dataset-disclosure__intro")
            _refs["ops"] = ui.column().classes("w-full sov-dataset-operator")
            with ui.row().classes("items-center w-full sov-dataset-index-controls"):
                action_button(
                    "Запустить очередь",
                    icon="o_play_arrow",
                    on_click=_start_all,
                    variant="primary",
                    compact=True,
                )
                action_button(
                    "Остановить",
                    icon="o_pause",
                    on_click=_stop_all,
                    variant="secondary",
                    compact=True,
                )
                _refs["status"] = ui.label("Индексатор: …").classes("sov-dataset-index-status")
                ui.element("div").classes("sov-flex-spacer")
                action_button(
                    "Обновить состояние",
                    icon="o_refresh",
                    on_click=_refresh,
                    variant="quiet",
                    compact=True,
                )
            _refs["settings"] = {}
            with ui.expansion(
                "Тонкая настройка партий и памяти",
                icon="o_tune",
                value=False,
            ).classes("w-full sov-dataset-settings"):
                ui.label(
                    "Стандартные значения безопаснее для памяти. Меняйте их только для конкретной задачи."
                ).classes("sov-dataset-settings__note")
                with ui.element("div").classes("sov-dataset-settings__grid"):
                    batch_in = ui.number("Файлов в партии", value=_setting("batch_limit"), min=1, max=25, step=1).props(
                        "dense outlined"
                    )
                    max_in = ui.number("Максимум партий", value=_setting("max_batches"), min=1, max=500, step=1).props(
                        "dense outlined"
                    )
                    cooldown_in = ui.number("Пауза, секунд", value=_setting("cooldown_sec"), min=0, max=600, step=5).props(
                        "dense outlined"
                    )
                    min_ram_in = ui.number("Минимум RAM, ГБ", value=_setting("min_free_gb"), min=1, max=64, step=1).props(
                        "dense outlined"
                    )
                    swap_in = ui.number("Swap, %", value=_setting("max_swap_pct"), min=0, max=100, step=5).props(
                        "dense outlined"
                    )
                    row_batch_in = ui.number("Ручная партия", value=_setting("row_batch_limit"), min=1, max=25, step=1).props(
                        "dense outlined"
                    )
                with ui.row().classes("items-center w-full sov-dataset-settings__switches"):
                    if sys.platform != "win32":
                        unload_between_sw = ui.switch(
                            "Выгружать MLX между партиями",
                            value=_setting("unload_between_batches"),
                        )
                        unload_before_sw = ui.switch(
                            "Выгрузить MLX перед стартом",
                            value=_setting("unload_before_start"),
                        )
                        unload_between_sw.on_value_change(
                            lambda *_: _set_setting("unload_between_batches", unload_between_sw.value)
                        )
                        unload_before_sw.on_value_change(
                            lambda *_: _set_setting("unload_before_start", unload_before_sw.value)
                        )
                        _refs["settings"]["unload_between_batches"] = unload_between_sw
                        _refs["settings"]["unload_before_start"] = unload_before_sw
                    else:
                        ui.label("Используется Ollama / облачный провайдер — выгрузка MLX не требуется").classes("sov-dataset-settings__note")
                    ui.element("div").classes("sov-flex-spacer")
                    action_button(
                        "По умолчанию",
                        icon="o_restart_alt",
                        on_click=_reset_index_settings,
                        variant="quiet",
                        compact=True,
                    )
                _refs["settings"].update({
                    "batch_limit": batch_in,
                    "max_batches": max_in,
                    "cooldown_sec": cooldown_in,
                    "min_free_gb": min_ram_in,
                    "max_swap_pct": swap_in,
                    "row_batch_limit": row_batch_in,
                })
                batch_in.on_value_change(lambda *_: _set_setting("batch_limit", batch_in.value))
                max_in.on_value_change(lambda *_: _set_setting("max_batches", max_in.value))
                cooldown_in.on_value_change(lambda *_: _set_setting("cooldown_sec", cooldown_in.value))
                min_ram_in.on_value_change(lambda *_: _set_setting("min_free_gb", min_ram_in.value))
                swap_in.on_value_change(lambda *_: _set_setting("max_swap_pct", swap_in.value))
                row_batch_in.on_value_change(lambda *_: _set_setting("row_batch_limit", row_batch_in.value))
        operator_disclosure.set_visibility(can_manage)

        with panel(variant="plain", classes="sov-dataset-registry-panel"):
            with ui.row().classes("items-center w-full sov-dataset-section-head"):
                section_heading(
                    "Наборы данных",
                    "Найдите набор, проверьте его состав и откройте нужные файлы.",
                )
                ui.element("div").classes("sov-flex-spacer")
                action_button(
                    icon="o_refresh",
                    on_click=_refresh,
                    variant="quiet",
                    compact=True,
                    icon_only=True,
                    aria_label="Обновить список датасетов",
                    classes="sov-dataset-refresh",
                )
            _refs["fbtn"] = {}
            with ui.element("div").classes("sov-dataset-toolbar"):
                _fsearch = text_field(
                    placeholder="Найти набор данных",
                    clearable=True,
                    classes="sov-dataset-search",
                )
                _fsearch.on_value_change(lambda *_: (_S.update(q=(_fsearch.value or "")), _render_rows()))
                with ui.row().classes("items-center sov-dataset-filters"):
                    for _fk, _flbl in (
                        ("all", "Все"),
                        ("indexed", "Готовы"),
                        ("pending", "Ждут"),
                        ("error", "Ошибки"),
                        ("empty", "Пустые"),
                    ):
                        active_class = " sov-dataset-filter--active" if _fk == "all" else ""
                        _refs["fbtn"][_fk] = action_button(
                            _flbl,
                            on_click=lambda k=_fk: _set_filter(k),
                            variant="quiet",
                            compact=True,
                            classes=f"sov-dataset-filter{active_class}",
                        )
            _refs["disp"] = ui.column().classes("w-full sov-dataset-results")

    ui.timer(0.1, _refresh, once=True)
    # Авто-обновление: статус индексатора часто и дёшево, полная сводка (счётчики+строки) реже
    status_timer = ui.timer(5.0, _refresh_status) if can_manage else None
    refresh_timer = ui.timer(20.0, _refresh)
    return {"timers": [timer for timer in (status_timer, refresh_timer) if timer is not None]}


