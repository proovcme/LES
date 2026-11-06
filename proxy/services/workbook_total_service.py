"""Verified sums of explicitly selected Excel cells for a single chat request."""
from __future__ import annotations

import asyncio
from decimal import Decimal, Inexact, localcontext
import re

from openpyxl.utils.cell import coordinate_to_tuple, get_column_letter
from openpyxl.worksheet.cell_range import CellRange

from backend.interface import Chunk
from proxy.services.document_original_service import resolve_original
from proxy.services.tabular_document_service import MAX_CELLS, MAX_COLUMNS, MAX_ROWS, read_table

TOOL_NAME = "sum_excel_ranges"
EXCEL_SUFFIXES = (".xlsx", ".xlsm")
MAX_RANGES = 8
_A1 = re.compile(r"\$?[A-Z]{1,3}\$?[1-9][0-9]{0,4}(?::\$?[A-Z]{1,3}\$?[1-9][0-9]{0,4})?", re.IGNORECASE)


def calculate_ranges(document, ranges):
    """Return one subtotal per range. A failed cell invalidates the entire sum."""
    if not 1 <= len(ranges) <= MAX_RANGES:
        raise ValueError(f"Укажите от 1 до {MAX_RANGES} диапазонов")
    sheets = {sheet["name"]: sheet for sheet in document["sheets"]}
    selected = []
    requested_cells = 0
    for item in ranges:
        name, address = item["sheet"], item["range"]
        if name not in sheets or not _A1.fullmatch(address):
            raise ValueError("Укажите точное имя листа и адрес A1 или A1:B10")
        region = CellRange(address.upper())
        if region.max_row > MAX_ROWS or region.max_col > MAX_COLUMNS:
            raise ValueError("Диапазон превышает предел чтения таблицы")
        if any(sheet == name and not prior.isdisjoint(region) for sheet, prior, _ in selected):
            raise ValueError("Диапазоны на одном листе пересекаются")
        if any(not CellRange(merged).isdisjoint(region) for merged in sheets[name]["merged_ranges"]):
            raise ValueError(f"{name}!{region.coord} содержит объединённые ячейки")
        size = region.size["rows"] * region.size["columns"]
        requested_cells += size
        if requested_cells > MAX_CELLS:
            raise ValueError("Суммарный диапазон превышает 300000 ячеек")
        selected.append((name, region, size))

    def display(value):
        value = value.normalize()
        return format(value, "f") if -12 <= value.adjusted() <= 20 else str(value)

    with localcontext() as context:
        # Excel finite numbers have at most 309 integer and 324 fractional places.
        context.prec = 1024
        context.traps[Inexact] = True
        subtotals = []
        total = Decimal(0)
        for name, region, size in selected:
            subtotal = Decimal(0)
            numeric = cached = 0
            for row in document["rows"]:
                if row["sheet"] != name or not region.min_row <= row["row"] <= region.max_row:
                    continue
                for cell in row["cells"]:
                    _, column = coordinate_to_tuple(cell["coordinate"])
                    if not region.min_col <= column <= region.max_col:
                        continue
                    location = f"{name}!{cell['coordinate']}"
                    value = cell
                    if cell["kind"] == "formula":
                        value = cell.get("cached")
                        if cell.get("needs_recalculation") or not value or value["kind"] == "error":
                            raise ValueError(f"У {location} нет сохранённого результата формулы")
                        cached += 1
                    if value["kind"] != "number":
                        raise ValueError(f"{location} содержит значение, которое нельзя сложить как число")
                    subtotal += Decimal(value["value"])
                    numeric += 1
            subtotals.append({
                "sheet": name, "range": region.coord, "first_row": region.min_row,
                "sum": display(subtotal), "numeric_cells": numeric,
                "blank_cells": size - numeric, "cached_formulas": cached,
            })
            total += subtotal
        return {"total": display(total), "ranges": subtotals, "source_sha256": document["sha256"]}


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
            last_rows = {}
            for row in document["rows"]:
                last_rows[row["sheet"]] = max(last_rows.get(row["sheet"], 0), row["row"])
            return {"sheets": [{"name": sheet["name"], "last_row": last_rows.get(sheet["name"], 0),
                                "last_column": get_column_letter(sheet["columns"]), "hidden": sheet["hidden"]}
                               for sheet in document["sheets"]]}
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
