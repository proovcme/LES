"""Read exact numbered headings from one version of an indexed document."""
from __future__ import annotations

import asyncio
import re

from fastapi import HTTPException

from backend.index_replacement import ReplacementJournal
from backend.interface import Chunk, EmbeddingContractError
from proxy.services.dataset_runtime import get_dataset_state
from proxy.services.document_explorer_service import explorer
from proxy.services.lexical_evidence_service import canonicalize_lexical_hits
from proxy.services.source_locator_service import source_map_item

CLAUSE_NUMBER = r"^[0-9]{1,5}(?:\.[0-9]{1,5}){1,5}$"


async def read_clause(doc_id: str, number: str, dataset_id: str = "", doc_name: str = "",
                      *, max_chars: int = 24000) -> dict:
    if not re.fullmatch(CLAUSE_NUMBER, number):
        raise HTTPException(422, "Укажите номер пункта, например 15.3.")
    service = explorer()
    if doc_id:
        document = await asyncio.to_thread(service.get_document, doc_id)
    else:
        document = await asyncio.to_thread(service.get_document_by_source, dataset_id, doc_name)
    if not document or document["status"] == "MISSING" or (dataset_id and document["dataset_id"] != dataset_id):
        raise HTTPException(404, "Индексированный оригинал недоступен.")
    dataset_id, name = document["dataset_id"], document["file_name"]
    backend = get_dataset_state().backend
    journal = ReplacementJournal.for_adapter(backend)
    try:
        stamp = await asyncio.to_thread(journal.read_stamp)
        rows = await asyncio.to_thread(service.numbered_clause, dataset_id, name, number)
        if len(rows) > 24 or sum(len(row["text"]) for row in rows) > max_chars:
            raise ValueError("Пункт не помещается в окно чтения. Откройте оригинал документа.")
        seeds = [Chunk(row["text"], row["doc_id"], name, 0.0,
                       {"point_id": row["point_id"], "dataset_id": dataset_id}) for row in rows]
        chunks = await canonicalize_lexical_hits(seeds, backend, [dataset_id], [name])
        await asyncio.to_thread(journal.assert_unchanged, stamp)
    except ValueError as error:
        raise HTTPException(422, str(error)) from error
    except EmbeddingContractError as error:
        raise HTTPException(409, "Индекс меняется. Повторите чтение пункта.") from error
    if not chunks:
        return {"status": "not_found", "number": number, "sources": []}
    headings = {str(chunk.meta.get("section_heading") or chunk.meta.get("parent_heading") or "").strip()
                for chunk in chunks}
    revisions = {chunk.meta.get("source_sha256") for chunk in chunks}
    if len(headings) > 1 or len(revisions) > 1:
        return {"status": "ambiguous", "number": number, "sources": []}
    return {
        "status": "found", "number": number, "document_id": document["id"],
        "coverage": "indexed_fragments",
        "sources": [source_map_item(chunk, index=index, excerpt_chars=len(chunk.content))
                    for index, chunk in enumerate(chunks, 1)],
    }
