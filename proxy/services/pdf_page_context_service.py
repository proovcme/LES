"""Bounded neighbor reads for already indexed PDF pages."""
from __future__ import annotations

import asyncio
from collections import defaultdict
from dataclasses import dataclass, field
import logging
from pathlib import PurePath

from qdrant_client import models

from backend.interface import Chunk, EmbeddingContractError
from proxy.services.pdf_table_continuation_service import continuation_number, read_continuation

logger = logging.getLogger(__name__)
MAX_ANCHOR_PAGES = 4
MAX_PAGE_POINTS = 64
MAX_EDGE_PARTS = 2
MAX_EDGE_CHUNKS = 12


def _page_key(payload):
    page = payload.get("page") if payload.get("type") == "pdf_page_text" else payload.get("source_page")
    name = str(payload.get("file_name") or "")
    if (payload.get("type") not in {"pdf_page_text", "table_row"}
            or type(page) is not int or page < 1 or PurePath(name).suffix.casefold() != ".pdf"):
        return None
    return str(payload.get("dataset_id") or ""), name, page


@dataclass
class PdfPageContext:
    backend: object
    pages: dict = field(default_factory=dict)
    tables: dict = field(default_factory=dict)

    async def _scroll(self, keys, limit):
        pages = defaultdict(list)
        for dataset_id, name, number in keys:
            pages[dataset_id, name].append(number)
        files = [
            models.Filter(must=[
                models.FieldCondition(key="dataset_id", match=models.MatchValue(value=dataset_id)),
                models.FieldCondition(key="file_name", match=models.MatchValue(value=name)),
                models.FieldCondition(key="page", match=models.MatchAny(any=numbers)),
            ])
            for (dataset_id, name), numbers in pages.items()
        ]
        scope = models.Filter(
            must=[models.FieldCondition(key="type", match=models.MatchValue(value="pdf_page_text"))],
            should=files,
            must_not=[
                models.FieldCondition(key="node_role", match=models.MatchValue(value="navigation")),
                models.FieldCondition(key="evidence_admissible", match=models.MatchValue(value=False)),
            ],
        )
        points, offset = await self.backend.aclient.scroll(
            collection_name=self.backend.collection_name, scroll_filter=scope,
            limit=limit, with_payload=True, with_vectors=False,
        )
        by_page = {key: [] for key in keys}
        for point in points:
            payload = point.payload or {}
            key = _page_key(payload)
            if (key not in by_page or payload.get("type") != "pdf_page_text"
                    or payload.get("node_role") == "navigation" or payload.get("evidence_admissible") is False):
                raise EmbeddingContractError("SECTION_SCOPE_MISMATCH")
            by_page[key].append(point)
        return by_page, offset is not None

    @staticmethod
    def _bounded(points):
        if len(points) > MAX_PAGE_POINTS:
            return None
        return sorted(points, key=lambda point: (int(point.payload.get("chunk_ord") or 0), str(point.id)))

    async def _load_one(self, key):
        by_page, truncated = await self._scroll([key], MAX_PAGE_POINTS + 1)
        return key, None if truncated else self._bounded(by_page[key])

    async def _load(self, keys):
        if len(keys) == 1:
            return dict([await self._load_one(keys[0])])
        by_page, truncated = await self._scroll(keys, len(keys) * (MAX_PAGE_POINTS + 1))
        if truncated:
            # A large page can fill the shared result ahead of smaller pages.
            # Read each page independently to keep the original per-page limit.
            return dict(await asyncio.gather(*(self._load_one(key) for key in keys)))
        return {key: self._bounded(points) for key, points in by_page.items()}

    async def read(self, seeds, covered_ids):
        anchors = list(dict.fromkeys(key for payload in seeds if (key := _page_key(payload))))
        if not anchors:
            return [], {}
        anchors = anchors[:MAX_ANCHOR_PAGES]
        edges = list(dict.fromkeys(
            (dataset, name, adjacent)
            for dataset, name, page in anchors
            for adjacent in (page + 1, page - 1)
            if adjacent > 0
        ))
        needed = [key for key in dict.fromkeys([*anchors, *edges]) if key not in self.pages]
        if needed:
            self.pages.update(await self._load(needed))

        # Confirm at most one explicitly marked table continuation per request.
        pairs = dict.fromkeys(
            (dataset, name, first)
            for dataset, name, page in anchors
            for first in (page, page - 1)
            if first > 0
        )
        table_chunks = []
        for dataset, name, first in pairs:
            if (dataset, name, first) not in self.tables and self.tables:
                continue
            before = self.pages.get((dataset, name, first))
            after = self.pages.get((dataset, name, first + 1))
            if not before or not after:
                continue
            number = continuation_number(
                "\n".join(point.payload["text"] for point in before),
                "\n".join(point.payload["text"] for point in after),
            )
            if not number:
                continue
            key = dataset, name, first
            if key not in self.tables:
                try:
                    self.tables[key] = await asyncio.to_thread(read_continuation, self.backend, *key, number)
                except Exception as error:
                    logger.warning("[PDF_TABLE] continuation read failed: %s", type(error).__name__)
                    self.tables[key] = []
            table_chunks = self.tables[key]
            break

        added = []
        seen = set(covered_ids)
        for dataset, name, page in anchors:
            following = self.pages.get((dataset, name, page + 1)) or []
            preceding = self.pages.get((dataset, name, page - 1)) or []
            for point in [*following[:MAX_EDGE_PARTS], *preceding[-MAX_EDGE_PARTS:]]:
                point_id = str(point.id)
                if point_id in seen or len(added) >= MAX_EDGE_CHUNKS:
                    continue
                seen.add(point_id)
                payload = point.payload
                added.append(Chunk(str(payload["text"]), str(payload.get("doc_id") or ""), name, 0.0, {
                    **payload, "qdrant_point_id": point_id, "context_origin": "pdf_boundary_read",
                    "atomic_evidence": True,
                }))
        return [*table_chunks, *added], {
            "anchor_pages": len(anchors), "adjacent_pages_read": sum(bool(self.pages[key]) for key in edges),
            "adjacent_pages_over_limit": sum(self.pages[key] is None for key in edges),
            "added_fragments": len(added), "confirmed_table_pages": len(table_chunks),
        }
