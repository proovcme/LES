"""Read attachments with row provenance and preserved server-owned originals."""
import asyncio
import logging
import os
from pathlib import Path
from typing import Any
from fastapi import HTTPException
logger = logging.getLogger(__name__)
_READ_ATTACH_MAX_CHARS = int(os.getenv("RAG_ATTACH_READ_MAX_CHARS", "18000"))

def _compact_cell(value: Any, *, max_len: int = 240) -> str:
    text = " ".join(str("" if value is None else value).replace("\u00a0", " ").split())
    if len(text) > max_len:
        return text[: max_len - 1].rstrip() + "…"
    return text

def _format_tabular_attachment_context(path: Path, original_name: str, *, max_chars: int) -> tuple[str, bool] | None:
    """Literal coordinates and whole-row preview; no lossy cell compaction."""
    from proxy.services.tabular_document_service import TABLE_SUFFIXES, read_table, preview
    if path.suffix.lower() not in TABLE_SUFFIXES:
        return None
    return preview(read_table(path, original_name), max_chars)

async def _prepare_read_attachment(
    temp_path: Path,
    original_name: str,
    *,
    attachment_id: str | None = None,
) -> dict[str, Any]:
    """Convert and preserve one lossless, server-owned read attachment."""
    from uuid import uuid4

    attach_id = attachment_id or f"read_{uuid4().hex[:12]}"
    from backend.product_edition import is_light
    from proxy.services.chat_attachment_service import IMAGE_SUFFIXES, image_data_url, preserve_read_attachment
    if is_light() and Path(original_name).suffix.lower() in IMAGE_SUFFIXES:
        try:
            await asyncio.to_thread(image_data_url, temp_path)
        except (OSError, ValueError) as error:
            raise HTTPException(422, f"Не удалось прочитать изображение: {error}") from error
        await asyncio.to_thread(preserve_read_attachment, temp_path, attachment_id=attach_id, original_name=original_name)
        return {"attachment_id": attach_id, "mode": "read", "name": original_name,
                "chars": 0, "text": "", "truncated": False, "media_type": "image"}
    try:
        structured = await asyncio.to_thread(
            _format_tabular_attachment_context,
            temp_path,
            original_name,
            max_chars=_READ_ATTACH_MAX_CHARS,
        )
    except Exception as error:  # Malformed ZIP/XML/CSV must remain a visible upload error.
        raise HTTPException(422, f'Не удалось прочитать таблицу «{original_name}»: {error}') from error
    if structured:
        text, truncated = structured
    else:
        from backend.converter import convert_to_markdown

        try:
            text = await asyncio.to_thread(convert_to_markdown, temp_path)
        except Exception as error:  # noqa: BLE001 - upload errors must stay operator-visible
            logger.warning("[ATTACH] read conversion failed for %s: %s", original_name, error)
            raise HTTPException(
                422,
                f"Не удалось прочитать файл «{original_name}»: {error}. "
                "Попробуй режим индексации/OCR или другой формат файла.",
            ) from error
        text = (text or "").strip()
        truncated = len(text) > _READ_ATTACH_MAX_CHARS

    text = (text or "").strip()
    suffix = Path(original_name).suffix.lower()
    if not text:
        raise HTTPException(
                422,
                f"Не удалось прочитать текст из «{original_name}». "
                "Для таблиц попробуй режим быстрой сверки, для сканов — индексацию/OCR.",
        )

    from proxy.services.chat_attachment_service import cleanup_expired, preserve_read_attachment

    await asyncio.to_thread(cleanup_expired)
    await asyncio.to_thread(
        preserve_read_attachment,
        temp_path,
        attachment_id=attach_id,
        original_name=original_name,
    )
    return {
        "attachment_id": attach_id,
        "mode": "read",
        "name": original_name,
        "chars": len(text),
        "text": text[:_READ_ATTACH_MAX_CHARS],
        "truncated": truncated,
    }
