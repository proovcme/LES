"""Read a confirmed two-page table from the indexed version of a PDF."""
from __future__ import annotations

from hashlib import sha256
from io import BytesIO
import re

from backend.inference.lexical_tokens import normalized_text
from backend.interface import Chunk
from backend.pdf_reader import complex_geometry_pages
from proxy.services.document_original_service import resolve_original

_NUMBER = r"([A-Za-zА-Яа-я]?\.?\d+(?:\.\d+)*[A-Za-zА-Яа-я]?)"
_TITLE = re.compile(rf"(?im)^\s*(?:таблица|table)[ \t]+{_NUMBER}(?!\w|\.\d|[ \t]*\((?:продолжение|continued)\))")
_CONTINUED = re.compile(
    rf"(?im)^\s*(?:продолжение[ \t]+таблицы[ \t]+{_NUMBER}(?!\w|\.\d)"
    rf"|(?:таблица|table)[ \t]+{_NUMBER}[ \t]*\((?:продолжение|continued)\))"
)
MAX_BYTES = 64 * 1024 * 1024
MAX_PAGE_CHARS = 4200


def continuation_number(before: str, after: str) -> str:
    """A repeated header alone is insufficient to identify a continuation."""
    titles = {normalized_text(match[1]) for match in _TITLE.finditer(before)}
    previous = {normalized_text(match[1] or match[2]) for match in _CONTINUED.finditer(before)}
    continued = {normalized_text(match[1] or match[2]) for match in _CONTINUED.finditer(after)}
    possible = titles | previous
    if len(possible) == 1 and continued == possible and not _TITLE.search(after):
        return next(iter(possible))
    return ""


def _single_table(page):
    if len(page.edges) > 2000:
        return None
    tables = page.find_tables()
    if len(tables) != 1:
        return None
    table = tables[0]
    cells = table.rows[0].cells
    if len(cells) < 2 or any(cell is None for cell in cells):
        return None
    left, _, right, _ = table.bbox
    if right <= left:
        return None
    columns = tuple((cell[2] - left) / (right - left) for cell in cells)
    matrix = table.extract()
    if not matrix or any(len(row) != len(columns) for row in matrix):
        return None
    return columns, matrix


def read_continuation(backend, dataset_id, file_name, first_page, number):
    """Recheck caption and column geometry. Keep each page as separate evidence."""
    lookup = getattr(getattr(backend, "db", None), "document_integrity_row", None)
    source = lookup(dataset_id, file_name) if lookup else None
    if not source or source["status"] != "INDEXED" or not source.get("file_hash"):
        return []
    path = resolve_original({**source, "dataset_id": dataset_id}, content_dir=backend.content_dir)
    if path is None:
        return []
    with path.open("rb") as file:
        raw = file.read(MAX_BYTES + 1)
    if len(raw) > MAX_BYTES or sha256(raw).hexdigest() != source["file_hash"]:
        return []
    pages = (first_page, first_page + 1)
    if complex_geometry_pages(raw, pages=pages):
        return []
    import pdfplumber

    with pdfplumber.open(BytesIO(raw), pages=list(pages)) as document:
        if len(document.pages) != 2:
            return []
        before, after = document.pages
        if continuation_number(before.extract_text() or "", after.extract_text() or "") != number:
            return []
        first, second = _single_table(before), _single_table(after)
    if first is None or second is None or len(first[0]) != len(second[0]):
        return []
    if any(abs(left - right) > 0.025 for left, right in zip(first[0], second[0])):
        return []

    result = []
    for page, (_, rows) in zip(pages, (first, second)):
        label = f"Таблица {number}, страница {page}"
        if page != first_page:
            label = f"Продолжение таблицы {number}, страница {page}"
        body = "\n".join(" | ".join(str(cell or "").replace("\n", " ").replace("|", r"\|") for cell in row)
                         for row in rows)
        text = f"{label}\n{body}"
        if len(text) > MAX_PAGE_CHARS:
            return []
        result.append(Chunk(text, source["id"], file_name, 0.0, {
            "dataset_id": dataset_id, "page": page, "source_ref": f"{file_name}#page={page}",
            "source_version": source["file_hash"], "table_name": number,
            "context_origin": "pdf_table_continuation", "atomic_evidence": True,
            "support_scope": "original_pdf_table_page",
        }))
    return result
