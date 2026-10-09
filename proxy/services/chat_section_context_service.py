"""Generation-consistent parent reads for the main chat evidence pipeline."""
from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

from fastapi import HTTPException
from qdrant_client import models

from backend.index_replacement import ReplacementJournal
from backend.interface import Chunk, EmbeddingContractError


class SectionReadError(HTTPException):
    """An evidence-boundary failure must also stop the model's tool loop."""


def _meta(chunk):
    return getattr(chunk, "meta", {}) or {}


def _identity(chunk):
    meta = _meta(chunk)
    return (str(meta.get("dataset_id") or ""), str(chunk.doc_name),
            str(meta.get("parent_id") or ""))


def _point_id(chunk):
    meta = _meta(chunk)
    return str(meta.get("qdrant_point_id") or meta.get("point_id") or "")


def visible_chunks(chunks, source_map):
    """UI/history previews use the same evidence set that reached the model."""
    keys = {(str(source.get("dataset_id") or ""), str(source.get("doc_name") or ""),
             str(source.get("qdrant_point_id") or ""), str(source.get("quote") or ""))
            for source in source_map if isinstance(source, dict)}
    return [chunk for chunk in chunks
            if (_identity(chunk)[0], chunk.doc_name, _point_id(chunk), chunk.content) in keys]


@dataclass
class SectionContext:
    chunks: list[Any]
    sections: list[dict]
    input_count: int
    added_count: int = 0

    def payload(self):
        return {"schema": "les.chat-section-context.v1", "enabled": True,
                "input_count": self.input_count, "output_count": len(self.chunks),
                "selected_hits": len(self.chunks) - self.added_count,
                "omitted_hits": self.input_count - (len(self.chunks) - self.added_count),
                "added_count": self.added_count, "sections": self.sections,
                "citations": "one_per_fragment", "packing": "hits_before_neighbours"}


class ChatSectionReader:
    """One request-scoped cache; never combine different index generations."""

    def __init__(self, backend, dataset_ids):
        self.backend = backend
        self.allowed = frozenset(str(value) for value in dataset_ids)
        self.journal = ReplacementJournal.for_adapter(backend) if hasattr(backend, "content_dir") else None
        self.stamp = None
        self.started = False
        self.cache = {}

    def assert_current(self):
        if self.started and self.journal is not None:
            self.journal.assert_unchanged(self.stamp)

    def assert_for_inference(self):
        try:
            self.assert_current()
        except EmbeddingContractError as error:
            raise SectionReadError(409, detail={"code": str(error)}) from error

    def coverage(self, visible_sources):
        """Completeness of indexed sections in the actual model packet, not OCR."""
        result = []
        for key, points in self.cache.items():
            included = {str(source.get("qdrant_point_id")) for source in visible_sources
                        if (source.get("dataset_id"), source.get("doc_name"), source.get("parent_id")) == key}
            result.append({"dataset_id": key[0], "file_name": key[1], "parent_id": key[2],
                           "indexed_fragments": len(points), "model_fragments": len(included),
                           "omitted_fragments": len(points) - len(included),
                           "model_section_complete": len(included) == len(points)})
        return result

    async def expand(self, chunks, *, max_chunks=None, **_unused):
        try:
            async with asyncio.timeout(15):
                return await self._expand(chunks, max_chunks=max_chunks)
        except EmbeddingContractError as error:
            raise SectionReadError(409, detail={"code": str(error)}) from error
        except HTTPException:
            raise
        except Exception as error:
            raise SectionReadError(503, detail={"code": "SECTION_READ_UNAVAILABLE"}) from error

    async def _expand(self, chunks, *, max_chunks):
        seeds = list(chunks[:min(10, max_chunks or 10)])
        if not seeds:
            return SectionContext([], [], 0)
        if not self.started:
            self.stamp = self.journal.read_stamp() if self.journal is not None else None
            self.started = True
        self.assert_current()
        for seed in seeds:
            meta = _meta(seed)
            if _identity(seed)[0] not in self.allowed:
                raise EmbeddingContractError("SECTION_SCOPE_MISMATCH")
            if meta.get("node_role") == "navigation" or meta.get("evidence_admissible") is False:
                raise EmbeddingContractError("SECTION_SCOPE_MISMATCH")
            if "_index_revision" in meta and meta["_index_revision"] != self.stamp:
                raise EmbeddingContractError("INDEX_CHANGED_DURING_SEARCH")

        # Check original hits against current payloads before reading any siblings.
        ids = list(dict.fromkeys(_point_id(seed) for seed in seeds if _point_id(seed)))
        if ids:
            records = await self.backend.aclient.retrieve(
                collection_name=self.backend.collection_name, ids=ids,
                with_payload=True, with_vectors=False)
            current = {str(point.id): point.payload or {} for point in records}
            for seed in seeds:
                if not _point_id(seed):
                    continue
                payload = current.get(_point_id(seed), {})
                if (payload.get("text") != seed.content or
                    (str(payload.get("dataset_id") or ""), str(payload.get("file_name") or ""),
                     str(payload.get("parent_id") or "")) != _identity(seed) or
                    any(payload.get(key) != _meta(seed).get(key)
                        for key in ("page", "source_page", "source_ref", "chunk_ord"))):
                    raise EmbeddingContractError("INDEX_CHANGED_DURING_SEARCH")

        groups = {}
        for seed in seeds:
            if _identity(seed)[2] and _point_id(seed):
                groups.setdefault(_identity(seed), []).append(seed)
        sections, pending, counts = [], [], {}
        for key, hits in list(groups.items())[:8]:
            if key not in self.cache:
                scope = models.Filter(
                    must=[models.FieldCondition(key=name, match=models.MatchValue(value=value))
                          for name, value in zip(("dataset_id", "file_name", "parent_id"), key)],
                    must_not=[
                        models.FieldCondition(key="node_role", match=models.MatchValue(value="navigation")),
                        models.FieldCondition(key="evidence_admissible", match=models.MatchValue(value=False)),
                    ])
                points, offset, chars = [], None, 0
                while True:
                    page, offset = await self.backend.aclient.scroll(
                        collection_name=self.backend.collection_name, scroll_filter=scope,
                        offset=offset, limit=128, with_payload=True, with_vectors=False)
                    points.extend(page)
                    chars += sum(len(str((point.payload or {}).get("text") or "")) for point in page)
                    if len(points) > 2048 or chars > 2_000_000:
                        raise SectionReadError(422, detail={"code": "SECTION_READ_LIMIT"})
                    if offset is None:
                        break
                self.cache[key] = points
            points = self.cache[key]
            by_id = {str(point.id): point for point in points}
            if len(by_id) != len(points) or any(_point_id(hit) not in by_id for hit in hits):
                raise EmbeddingContractError("INDEX_CHANGED_DURING_SEARCH")
            counts[key] = len(points)
            sections.append({"dataset_id": key[0], "file_name": key[1], "parent_id": key[2],
                             "indexed_fragments": len(points), "read_complete": True})
            neighbours = []
            for point in points:
                payload = point.payload or {}
                actual_key = tuple(str(payload.get(name) or "") for name in ("dataset_id", "file_name", "parent_id"))
                if actual_key != key or payload.get("node_role") == "navigation" or payload.get("evidence_admissible") is False:
                    raise EmbeddingContractError("SECTION_SCOPE_MISMATCH")
                if str(point.id) in ids:
                    continue
                neighbours.append(Chunk(str(payload.get("text") or ""), str(payload.get("doc_id") or ""),
                    key[1], 0.0, {**payload, "qdrant_point_id": str(point.id),
                                 "context_origin": "parent_read", "atomic_evidence": True,
                                 "section_fragment_count": len(points)}))
            neighbours.sort(key=lambda item: (
                min(abs(int(_meta(item).get("chunk_ord") or 0) - int(_meta(hit).get("chunk_ord") or 0)) for hit in hits),
                int(_meta(item).get("chunk_ord") or 0), _point_id(item)))
            pending.append(neighbours)

        # Originals are reserved first. Siblings retain their own page, text and ID.
        result = [Chunk(seed.content, str(getattr(seed, "doc_id", "") or ""), seed.doc_name,
                        float(getattr(seed, "score", 0) or 0),
                        {**_meta(seed), "atomic_evidence": True, "context_origin": "search_hit",
                         "section_fragment_count": counts.get(_identity(seed))})
                  for seed in seeds]
        added = 0
        for position in range(max((len(items) for items in pending), default=0)):
            for items in pending:
                if position < len(items) and added < 512:
                    result.append(items[position])
                    added += 1
        self.assert_current()
        return SectionContext(result, sections, len(chunks), added)
