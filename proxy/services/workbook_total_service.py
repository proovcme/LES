"""Verified sums of explicitly selected Excel cells for a single chat request."""
from __future__ import annotations

from backend.workbook_calculation import execute as native_calculation

from backend.interface import Chunk
from proxy.services.tabular_document_service import read_table

TOOL_NAME = "sum_excel_ranges"
EXCEL_SUFFIXES = (".xlsx", ".xlsm")
MAX_RANGES = 8


def calculate_ranges(document, ranges):
    """The built-in Rust calculator rejects any unverified/non-numeric cell."""
    return native_calculation(document, action='sum', ranges=ranges)



def total_evidence(payload, candidates):
    if payload["status"] != "ok" or payload["operation"] != "sum":
        return ()
    result = payload["result"]
    source = next(item for item in candidates if item["id"] == result["doc_id"])
    chunks = []
    for item in result["ranges"]:
        sheet, address = item["sheet"], item["range"]
        content = (f"Расчёт по оригиналу. Лист «{sheet}», диапазон {address}. "
                   f"Сумма диапазона {item['sum']}. Числовых ячеек {item['numeric_cells']}, "
                   f"пустых {item['blank_cells']}, значений формул из файла {item['cached_formulas']}.")
        chunks.append(Chunk(content, source["id"], source["file_name"], 1.0, {
            "dataset_id": source["dataset_id"], "chunk_id": f"excel-sum:{source['id']}:{sheet}!{address}",
            "source_version": result["source_sha256"], "sheet": sheet, "row": item["first_row"], "cell": address,
            "atomic_evidence": True, "support_scope": "original_table_total",
        }))
    return tuple(chunks)
