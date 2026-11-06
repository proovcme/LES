"""Addressed evidence from indexed DOCX originals, in document order."""
from __future__ import annotations

from collections import Counter
from dataclasses import dataclass
from hashlib import sha256
from io import BytesIO
from itertools import groupby
from pathlib import PurePath
import re
import zipfile

from backend.inference.lexical_tokens import tokenize_current
from backend.interface import Chunk
from proxy.services.document_original_service import resolve_original

MAX_BYTES = 64 * 1024 * 1024
MAX_BLOCKS = 8000
MAX_EVIDENCE_BLOCKS = 8
MAX_BLOCK_CHARS = 2200


@dataclass(frozen=True)
class WordBlock:
    anchor: str
    label: str
    text: str
    section: tuple[int, ...]
    table: int | None = None


def _heading_level(paragraph) -> int | None:
    explicit = paragraph._p.xpath("./w:pPr/w:outlineLvl/@w:val")
    if explicit and re.fullmatch(r"[0-8]", explicit[0]):
        return int(explicit[0]) + 1
    style = paragraph.style
    name = str(style.name or "") if style is not None else ""
    match = re.fullmatch(r"(?:heading|заголовок)\s*([1-9])", name, re.IGNORECASE)
    return int(match[1]) if match else None


def _table_rows(table, table_index, section):
    for row_index, row in enumerate(table.rows):
        cells, column = [], row.grid_cols_before + 1
        for _, group in groupby(row.cells, key=lambda cell: cell._tc):
            spanned = list(group)
            stop = column + len(spanned) - 1
            address = str(column) if column == stop else f"{column}–{stop}"
            cells.append(f"Столбец {address}: {spanned[0].text.strip()}")
            column = stop + 1
        if cells:
            heading = bool(row._tr.xpath("./w:trPr/w:tblHeader[not(@w:val='0' or @w:val='false')]"))
            label = f"Таблица {table_index + 1}, строка {row_index + 1}"
            if heading:
                label += " (шапка)"
            yield WordBlock(f"t{table_index}r{row_index}", label, "\n".join(cells), section, table_index)


def _blocks(document):
    headings, paragraph_index, table_index = [], 0, 0
    for block in document.iter_inner_content():
        if getattr(block, "_p", None) is not None:
            text = block.text.strip()
            index = paragraph_index
            paragraph_index += 1
            if not text:
                continue
            level = _heading_level(block)
            if level is not None:
                while headings and headings[-1][0] >= level:
                    headings.pop()
                headings.append((level, index))
            label = f"{'Заголовок' if level is not None else 'Абзац'} {index + 1}"
            yield WordBlock(f"para{index}", label, text, tuple(index for _, index in headings))
        else:
            yield from _table_rows(block, table_index, tuple(index for _, index in headings))
            table_index += 1


def _read_blocks(path, expected_hash):
    from docx import Document

    with path.open("rb") as stream:
        raw = stream.read(MAX_BYTES + 1)
    if len(raw) > MAX_BYTES or sha256(raw).hexdigest() != expected_hash:
        raise ValueError("Оригинал Word изменился или превышает предел чтения")
    with zipfile.ZipFile(BytesIO(raw)) as archive:
        entries = archive.infolist()
        if len(entries) > 10000 or sum(entry.file_size for entry in entries) > MAX_BYTES:
            raise ValueError("Распакованный документ превышает предел чтения")
    document = Document(BytesIO(raw))
    unaddressed = bool(document.element.body.xpath(
        ".//w:txbxContent | .//w:footnoteReference | .//w:endnoteReference | .//w:tbl//w:tbl "
        "| .//w:ins | .//w:del | .//w:sdt | .//w:drawing"
    ))
    blocks = []
    for block in _blocks(document):
        if len(blocks) >= MAX_BLOCKS:
            raise ValueError("Документ содержит слишком много абзацев или строк")
        blocks.append(block)
    return blocks, unaddressed


def _rank(blocks, question):
    terms = set(tokenize_current(question))
    matches = [(block, terms & set(tokenize_current(block.text))) for block in blocks]
    frequency = Counter(term for _, overlap in matches for term in overlap)
    identifiers = {term for term in frequency if any(char.isdigit() for char in term)
                   and any(char.isalpha() for char in term)}
    ranked = sorted(((block, overlap) for block, overlap in matches
                     if overlap and (not identifiers or identifiers & overlap)),
                    key=lambda item: sum(1 / frequency[term] for term in sorted(item[1])), reverse=True)
    return [block for block, _ in ranked]


def _select(blocks, question, fallback, limit):
    matched = _rank(blocks, question) or _rank(blocks, fallback)
    if not matched:
        return [], {}
    first = matched[0]
    if first.table is None:
        group = [block for block in blocks if block.section[:len(first.section)] == first.section]
    else:
        group = [block for block in blocks if block.table == first.table]
    section = [block for block in blocks if block.section[:len(first.section)] == first.section]
    context = section if len(section) <= limit else group
    chosen = []
    for block in dict.fromkeys([first, *context, *matched[1:3]] if len(context) <= limit
                               else [*matched[:3], *group]):
        if len(block.label) + len(block.text) <= MAX_BLOCK_CHARS:
            chosen.append(block)
        if len(chosen) == limit:
            break
    if len(context) <= limit and set(context) <= set(chosen):
        # Keep full small sections in reading order, including their headers.
        chosen = [*context, *(block for block in chosen if block not in context)]
    return chosen, {
        "group": "table" if first.table is not None else "section",
        "group_blocks": len(group),
        "included_group_blocks": len(set(group) & set(chosen)),
        "selected_blocks": len(chosen),
        "all_body_blocks_included": len(chosen) == len(blocks),
    }


def retrieve_word_evidence(backend, *, question, dataset_ids, file_names, index_hits):
    lookup = getattr(getattr(backend, "db", None), "document_integrity_row", None)
    if not lookup:
        return [], []
    allowed, selected = set(dataset_ids), set(file_names)
    sources = {}
    for hit in index_hits:
        dataset = (getattr(hit, "meta", {}) or {}).get("dataset_id")
        name = hit.doc_name
        if (dataset in allowed and PurePath(name).suffix.casefold() == ".docx"
                and (not selected or name in selected)):
            sources.setdefault((dataset, name), hit.content[:1200])
    # An explicit file choice still allows reading when the search missed.
    for name in file_names:
        if PurePath(name).suffix.casefold() == ".docx":
            for dataset in dataset_ids:
                sources.setdefault((dataset, name), "")
    originals = []
    for (dataset, name), fallback in sources.items():
        source = lookup(dataset, name)
        if not source or source["status"] != "INDEXED" or not source.get("file_hash"):
            continue
        if len(originals) >= 2:
            break
        originals.append((dataset, name, fallback, source))
    evidence, coverage = [], []
    limit = MAX_EVIDENCE_BLOCKS // max(1, len(originals))
    for dataset, name, fallback, source in originals:
        path = resolve_original({**source, "dataset_id": dataset}, content_dir=getattr(backend, "content_dir", None))
        try:
            if path is None:
                raise FileNotFoundError
            blocks, unaddressed = _read_blocks(path, source["file_hash"])
            chosen, status = _select(blocks, question, fallback, limit)
        except (ValueError, OSError, zipfile.BadZipFile):
            coverage.append({"file_name": name, "status": "unavailable"})
            continue
        coverage.append({"dataset_id": dataset, "file_name": name, "status": "read",
                         "unaddressed_content": unaddressed, **status})
        for block in chosen:
            evidence.append(Chunk(f"{block.label}\n{block.text}", source["id"], name, 0.0, {
                "dataset_id": dataset, "source_ref": f"{name}#{block.anchor}", "anchor": block.anchor,
                "chunk_id": f"word:{source['id']}:{block.anchor}", "source_version": source["file_hash"],
                "context_origin": "word_original_read", "atomic_evidence": True,
                "support_scope": "original_word_block",
            }))
    return evidence, coverage
