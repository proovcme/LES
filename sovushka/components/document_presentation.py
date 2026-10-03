"""Formatting and small UI primitives for the document browser."""
from __future__ import annotations

import asyncio
import re
from nicegui import ui

def _schedule(coro):
    asyncio.create_task(coro)

def _label(text: str, *, size: str = "12px", color: str = "var(--text)", weight: int = 500):
    return ui.label(text).style(f"font-size:{size};color:{color};font-weight:{weight};")

def _badge(text: str, cls: str = "tag-dim"):
    return ui.label(text).classes(cls)

def _format_size(value: int | float | str | None) -> str:
    try:
        n = float(value or 0)
    except (TypeError, ValueError):
        return "0 Б"
    units = ["Б", "КБ", "МБ", "ГБ"]
    i = 0
    while n >= 1024 and i < len(units) - 1:
        n /= 1024
        i += 1
    return f"{n:.1f} {units[i]}" if i else f"{int(n)} {units[i]}"

def _dataset_title(row: dict) -> str:
    name = str(row.get("display_name") or row.get("name") or row.get("id") or "")
    return name or "Без названия"

def _file_icon(file_name: str) -> str:
    suffix = str(file_name or "").lower().rsplit(".", 1)[-1]
    return {
        "pdf": "o_picture_as_pdf",
        "doc": "o_description",
        "docx": "o_description",
        "xls": "o_table_view",
        "xlsx": "o_table_view",
        "csv": "o_table_view",
        "dwg": "o_architecture",
        "dxf": "o_architecture",
        "ifc": "o_view_in_ar",
        "rvt": "o_view_in_ar",
        "msg": "o_mail",
        "eml": "o_mail",
    }.get(suffix, "o_draft")

def _file_kind(file_name: str) -> str:
    parts = str(file_name or "").rsplit(".", 1)
    return parts[-1].upper() if len(parts) > 1 else "Файл"

def _dataset_group(row: dict) -> str:
    kind = str(row.get("dataset_kind") or "").strip()
    if kind:
        return "project" if kind == "project" else "other"
    name = str(row.get("name") or row.get("id") or "").strip().upper()
    non_project_prefixes = (
        "ARTEL", "BOOKS", "CAD_BIM", "DOCS_OTHER", "EXPORTS", "GESN_", "GKRF",
        "MAIL", "NTD_", "SMETA_", "КАТАЛОГ",
    )
    return "other" if name.startswith(non_project_prefixes) else "project"

def _file_sort_key(item: dict) -> tuple[bool, str]:
    file_name = str(item.get("file_name") or "")
    basename = file_name.rsplit("/", 1)[-1]
    technical = basename.startswith(".") or basename.startswith("_les_")
    return technical, file_name.casefold()

def _plain_index_text(value: object) -> str:
    text = " ".join(str(value or "").split())
    text = re.sub(r"^#{1,6}\s*", "", text)
    text = re.sub(r"[*_`]+", "", text)
    return text.strip()

def _short_path(value: str, *, parts: int = 3) -> str:
    chunks = [x for x in str(value or "").split("/") if x]
    if len(chunks) <= parts:
        return str(value or "")
    return ".../" + "/".join(chunks[-parts:])

def _readiness_label(value: dict) -> tuple[str, str]:
    state_name = str(value.get("state") or "unknown")
    return {
        "ready": ("RRF готов", "tag-ok"),
        "awaiting_activation": ("Готов к включению", "tag-acc"),
        "building": ("Индексируется", "tag-acc"),
        "degraded": ("Режим деградации", "tag-warn"),
        "blocked": ("Не готов", "tag-warn"),
        "missing": ("Индекс отсутствует", "tag-warn"),
    }.get(state_name, ("Статус неизвестен", "tag-dim"))
