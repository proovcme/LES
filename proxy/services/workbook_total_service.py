"""Verified sums of explicitly selected Excel cells for a single chat request."""
from __future__ import annotations

import asyncio
from backend.workbook_calculation import execute as native_calculation

from backend.interface import Chunk
from proxy.services.document_original_service import resolve_original
from proxy.services.tabular_document_service import read_table

TOOL_NAME = "sum_excel_ranges"
EXCEL_SUFFIXES = (".xlsx", ".xlsm")
MAX_RANGES = 8


def calculate_ranges(document, ranges):
    """The built-in Rust calculator rejects any unverified/non-numeric cell."""
    return native_calculation(document, action='sum', ranges=ranges)



def register_workbook_totals(harness, backend, candidates):
    from proxy.services.tool_contract_service import (
        EffectClass, IdempotencyPolicy, ResultBudget, RetryPolicy, ToolContract,
    )
    from proxy.services.tool_harness_service import _result
    from proxy.services.tool_registry_service import ToolRegistration

    allowed = {source["id"]: source for source in candidates}

    def execute(args):
        operation = args["operation"]
        if operation == "list":
            return {"workbooks": [{"doc_id": key, "name": source["file_name"]}
                                  for key, source in allowed.items()]}
        source = allowed.get(args.get("doc_id"))
        if not source:
            raise ValueError("Выберите книгу из списка источников этого запроса")
        current = backend.db.document_integrity_row(source["dataset_id"], source["file_name"])
        if (not current or current["id"] != source["id"] or current["status"] != "INDEXED"
                or current["file_hash"] != source["file_hash"]):
            raise ValueError("Версия книги изменилась. Повторите запрос после обновления индекса")
        path = resolve_original({**current, "dataset_id": source["dataset_id"]},
                                content_dir=getattr(backend, "content_dir", None))
        if path is None:
            raise ValueError("Оригинал книги недоступен")
        document = read_table(path, name=source["file_name"])
        if document["sha256"] != source["file_hash"]:
            raise ValueError("Версия оригинала не совпадает с индексом")
        if operation == "describe":
            return native_calculation(document, action='describe')
        return {"doc_id": source["id"], **calculate_ranges(document, args.get("ranges", []))}

    async def handler(args):
        try:
            result = await asyncio.to_thread(execute, args)
        except (ValueError, OSError, ArithmeticError) as error:
            # Only emit expected validation messages. OS exceptions can contain local paths.
            reason = str(error) if isinstance(error, ValueError) else "Не удалось прочитать книгу или вычислить сумму"
            return _result(tool=TOOL_NAME, operation=args["operation"], inputs=[], status="error",
                           result={"reason": reason}, trace="workbook_total_rejected")
        return _result(tool=TOOL_NAME, operation=args["operation"], inputs=[], status="ok",
                       result=result, trace="workbook_original")

    contract = ToolContract(
        name=TOOL_NAME, version="1.0.0", title="Сумма по диапазонам Excel", category="source",
        summary="Calculate a sum from original XLSX/XLSM cells. List returns the permitted workbooks. "
                "Describe returns sheet names and used dimensions. Sum needs exact sheet names and "
                "explicit ranges (e.g. B2:B501). Include all intended ranges in one sum call per workbook. "
                "Ask for the intended ranges when unclear. "
                "Avoid mixing units or including subtotal rows unless requested. "
                "Formula values come from the saved cache. Cite the calculated range evidence.",
        input_schema={"type": "object", "properties": {
            "operation": {"type": "string", "enum": ["list", "describe", "sum"]},
            "doc_id": {"type": "string"},
            "ranges": {"type": "array", "minItems": 1, "maxItems": MAX_RANGES, "items": {
                "type": "object", "properties": {"sheet": {"type": "string"}, "range": {"type": "string"}},
                "required": ["sheet", "range"], "additionalProperties": False}},
        }, "required": ["operation"], "additionalProperties": False},
        result_schema="les_tool_result_v1", effect=EffectClass.READ, scopes=(), timeout_seconds=30,
        retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
        result_budget=ResultBudget(max_chars=16000, max_items=512), model_owned_fields=(),
        provenance="Version-checked workbook and explicit sheet ranges",
        tags=("excel", "xlsx", "xlsm", "sum"),
    )
    harness._registry.register(ToolRegistration(contract=contract, handler=handler))
    return contract.public_payload()


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
