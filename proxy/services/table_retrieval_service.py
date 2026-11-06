"""Find literal table evidence for the ordinary scoped RAG request."""
from __future__ import annotations

from collections import Counter
import logging
from pathlib import PurePath
from time import monotonic

from backend.inference.lexical_tokens import tokenize_current
from backend.interface import Chunk
from proxy.services.document_original_service import resolve_original
from proxy.services.tabular_document_service import TABLE_SUFFIXES, encode, read_table
from proxy.services.workbook_total_service import EXCEL_SUFFIXES

logger = logging.getLogger(__name__)
MAX_FILES = 4
MAX_EVIDENCE_ROWS = 6
MAX_ROW_CHARS = 1800
READ_BUDGET_SECONDS = 3


def table_source_key(chunk):
    meta = getattr(chunk, "meta", {}) or {}
    return str(meta.get("dataset_id") or ""), str(getattr(chunk, "doc_name", "") or "")


def _table_candidates(backend, dataset_ids, file_names, preferred_sources):
    find = getattr(getattr(backend, "db", None), "indexed_table_sources", None)
    if not find or not dataset_ids:
        return []
    return find(
        tuple(dict.fromkeys(dataset_ids)), sorted(TABLE_SUFFIXES),
        file_names=tuple(dict.fromkeys(file_names)), preferred_sources=preferred_sources, limit=MAX_FILES + 1,
    )


def _matched_rows(snapshot, terms):
    matches = []
    frequencies = Counter()
    for row in snapshot["rows"]:
        row_terms = set()
        for cell in row["cells"]:
            # Formula strings are not calculated values. Errors are not hits.
            value = cell["cached"] if cell["kind"] == "formula" else cell
            if value and value["kind"] in {"text", "number", "date"}:
                row_terms.update(tokenize_current(value["value"]))
        overlap = terms & row_terms
        if overlap:
            frequencies.update(overlap)
            matches.append((row, overlap))
    identifiers = {term for term in frequencies if any(char.isdigit() for char in term)
                   and any(char.isalpha() for char in term)}
    return [(row, sum(1 / frequencies[term] for term in sorted(overlap)), len(overlap))
            for row, overlap in matches if not identifiers or identifiers & overlap]


def _row_evidence(source, row, revision, score):
    cells = []
    for cell in row["cells"]:
        coordinate = cell["coordinate"]
        if cell["kind"] == "formula":
            value = cell["cached"]
            rendered = (f"сохранённое значение формулы {encode(value['value'])}"
                        if not cell["needs_recalculation"] else "результат формулы недоступен")
        elif cell["kind"] == "error":
            rendered = f"ошибка ячейки {encode(cell['value'])}"
        else:
            rendered = encode(cell["value"]) if cell["kind"] == "text" else str(cell["value"])
        cells.append(f"{coordinate} = {rendered}")
    location = row["cells"][0]["coordinate"]
    if len(cells) > 1:
        location += f":{row['cells'][-1]['coordinate']}"
    content = f"Лист {encode(row['sheet'])}, строка {row['row']}\n" + "\n".join(cells)
    if len(content) > MAX_ROW_CHARS:
        return None
    return Chunk(content, source["id"], source["file_name"], score, {
        "dataset_id": source["dataset_id"],
        "chunk_id": f"table:{source['id']}:{row['id']}",
        "source_version": revision,
        "sheet": row["sheet"],
        "row": row["row"],
        "cell": location,
        "atomic_evidence": True,
        "support_scope": "original_table_row",
    })


def retrieve_table_rows(backend, *, question, dataset_ids, file_names, index_hits):
    """Read a bounded set of originals, including when the index has no hit."""
    terms = set(tokenize_current(question))
    if not terms:
        return [], {}
    preferred_sources = tuple(dict.fromkeys(table_source_key(hit) for hit in index_hits))[:64]
    sources = _table_candidates(backend, dataset_ids, file_names, preferred_sources)
    if not sources:
        return [], {}
    deadline = monotonic() + READ_BUDGET_SECONDS
    coverage = {"read": 0, "sheets": 0, "matched_rows": 0, "omitted_long_rows": 0,
                "file_limit_reached": len(sources) > MAX_FILES, "time_limit_reached": False,
                "issues": [], "xlsx_sources": []}
    candidates = []
    for source in sources[:MAX_FILES]:
        if monotonic() >= deadline:
            coverage["time_limit_reached"] = True
            break
        try:
            path = resolve_original(source, content_dir=getattr(backend, "content_dir", None))
            if path is None:
                raise FileNotFoundError("Оригинал недоступен")
            snapshot = read_table(path, name=source["file_name"])
            if snapshot["sha256"] != source["file_hash"]:
                coverage["issues"].append("Версия оригинала не совпадает с индексом")
                continue
        except Exception as error:
            logger.warning("[TABLE_RAG] cannot read %s: %s", source["id"], type(error).__name__)
            coverage["issues"].append("Оригинал недоступен или превышает предел чтения")
            continue
        coverage["read"] += 1
        coverage["sheets"] += len(snapshot["sheets"])
        if PurePath(source["file_name"]).suffix.casefold() in EXCEL_SUFFIXES:
            coverage["xlsx_sources"].append(
                {key: source[key] for key in ("id", "dataset_id", "file_name", "file_hash")}
            )
        matches = _matched_rows(snapshot, terms)
        coverage["matched_rows"] += len(matches)
        # Preserve workbook order on a tie but spread ties across sheets.
        matches.sort(key=lambda item: (item[1], item[2], -item[0]["row"]), reverse=True)
        selected = 0
        for row, score, overlap in matches:
            chunk = _row_evidence(source, row, snapshot["sha256"], score)
            if chunk is None:
                coverage["omitted_long_rows"] += 1
                continue
            candidates.append((overlap, chunk))
            selected += 1
            if selected >= MAX_EVIDENCE_ROWS:
                break
    candidates.sort(key=lambda item: (item[1].score, item[0]), reverse=True)
    chunks = [chunk for _, chunk in candidates[:MAX_EVIDENCE_ROWS]]
    coverage["selected_rows"] = len(chunks)
    return chunks, coverage
