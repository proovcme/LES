"""Frozen pre-native row routines, used only as a generic table parity oracle."""
import json
from typing import Optional
from backend.parquet_writer import STANDARD_SCHEMA,DOC_TYPES

def _safe_float(v) -> Optional[float]:
    if v is None:
        return None
    try:
        s = str(v).replace(",", ".").replace(" ", "").replace("\xa0", "")
        return float(s)
    except Exception:
        return None

def normalize_row(raw_row: dict, mapping: dict, doc_type: str, doc_title: str, source_file: str) -> dict:
    """Нормализует одну строку Excel в стандартную схему."""
    norm = dict(STANDARD_SCHEMA)
    norm["doc_type"] = doc_type
    norm["doc_title"] = doc_title
    norm["source_file"] = source_file
    norm["raw_row"] = json.dumps(raw_row, ensure_ascii=False, default=str)
    for extra_key in ("source_page", "table_index", "needs_ocr", "extractor"):
        if extra_key in raw_row:
            norm[extra_key] = raw_row[extra_key]

    for orig_col, std_field in mapping.items():
        if std_field == "skip" or std_field not in norm:
            continue
        val = raw_row.get(orig_col)
        if val is None:
            continue

        # Числовые поля
        if std_field in ("qty", "qty_per_unit", "price", "amount", "amount_mat",
                         "amount_work", "work_volume", "work_done", "work_since_start",
                         "weight_unit", "weight_total"):
            norm[std_field] = _safe_float(val)
        else:
            norm[std_field] = str(val).strip() if val is not None else ""

    return norm

def row_to_chunk_text(row: dict) -> str:
    """
    Формирует текст чанка из нормализованной строки.
    Текст должен быть богатым — эмбеддинг по нему строится.
    """
    parts = []

    if row.get("doc_type") and row.get("doc_title"):
        parts.append(f"Документ: {DOC_TYPES.get(row['doc_type'], row['doc_type'])} — {row['doc_title']}")

    if row.get("section"):
        parts.append(f"Раздел: {row['section']}")

    if row.get("pos") or row.get("position"):
        parts.append(f"Позиция: {row.get('pos') or row.get('position')}")

    if row.get("name") or row.get("work_name"):
        parts.append(f"Наименование: {row.get('name') or row.get('work_name')}")

    if row.get("code") or row.get("designation"):
        parts.append(f"Обозначение/Артикул: {row.get('code') or row.get('designation')}")

    if row.get("mark"):
        parts.append(f"Марка/Тип: {row['mark']}")

    if row.get("unit") and row.get("qty") is not None:
        parts.append(f"Количество: {row['qty']} {row['unit']}")

    if row.get("price") is not None:
        parts.append(f"Цена: {row['price']:.2f}")

    if row.get("amount") is not None:
        parts.append(f"Сумма: {row['amount']:.2f}")

    if row.get("work_done") is not None:
        parts.append(f"Выполнено: {row['work_done']}")

    if row.get("norms_refs"):
        parts.append(f"Нормативы: {row['norms_refs']}")

    if row.get("materials_used"):
        parts.append(f"Материалы: {row['materials_used']}")

    if row.get("note"):
        parts.append(f"Примечание: {row['note']}")

    if (len(parts) <= 1 or row.get("sheet_name")) and row.get("raw_row"):
        try:
            raw = json.loads(row["raw_row"])
        except Exception:
            raw = {}
        raw_parts = []
        for key, value in raw.items():
            if value is None or str(value).strip() == "":
                continue
            raw_parts.append(f"{key}: {str(value).strip()}")
            if len("; ".join(raw_parts)) > 1200:
                break
        if raw_parts:
            parts.append("Строка: " + "; ".join(raw_parts))

    return "\n".join(parts) if parts else json.dumps(row, ensure_ascii=False)

def rows_to_qdrant_chunks(rows: list, dataset_id: str = "") -> list:
    """
    Конвертирует нормализованные строки в чанки для Qdrant.
    Возвращает list[dict] с полями "text" и "metadata".
    """
    chunks = []
    for row in rows:
        text = row_to_chunk_text(row)
        if not text.strip():
            continue

        # Payload для фильтрации в Qdrant
        metadata = {
            "type": "table_row",
            "doc_type": row.get("doc_type", ""),
            "doc_title": row.get("doc_title", ""),
            "source_file": row.get("source_file", ""),
            "source_page": row.get("source_page"),
            "table_index": row.get("table_index"),
            "needs_ocr": row.get("needs_ocr", False),
            "extractor": row.get("extractor", ""),
            "sheet_name": row.get("sheet_name"),
            "row": row.get("source_row"),
            "section": row.get("section", ""),
            "name": row.get("name", "") or row.get("work_name", ""),
            "code": row.get("code", "") or row.get("designation", ""),
            "unit": row.get("unit", ""),
            "qty": row.get("qty"),
            "amount": row.get("amount"),
            "dataset_id": dataset_id,
        }

        chunks.append({
            "text": text,
            "metadata": metadata,
            "raw": row,  # полная нормализованная строка
        })
    return chunks
