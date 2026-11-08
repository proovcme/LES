"""Read explicitly selected indexed units and verify their original file bytes."""
from __future__ import annotations

import asyncio
from collections import OrderedDict
from pathlib import Path

from qdrant_client import models

from backend.index_replacement import ReplacementJournal
from proxy.services.document_task_store import digest


def validate_selector(selector, allowed, files=()):
    from proxy.services.document_task_store import native_request
    return native_request('review_selector', dict(selector=selector, allowed=sorted(allowed), files=sorted(files)))


class ReviewSourceReader:
    def __init__(self, backend, dataset_ids, file_names=()):
        self.backend = backend
        self.allowed = frozenset(str(item) for item in dataset_ids)
        self.files = frozenset(file_names)
        self.journal = ReplacementJournal.for_adapter(backend) if hasattr(backend, 'content_dir') else None
        self.cache = OrderedDict()

    def _original(self, selector):
        from proxy.services.document_task_store import native_request
        row = self.backend.db.document_integrity_row(selector['dataset_id'], selector['file_name'])
        return native_request('review_original', dict(document={**selector, **(row or {})},
            content_root=str(Path(getattr(self.backend, 'content_dir', '.')).resolve())))

    async def read(self, selector, *, originals=None):
        selector = validate_selector(selector, self.allowed, self.files)
        stamp = self.journal.read_stamp() if self.journal else None
        key = (selector['dataset_id'], selector['file_name'])
        originals = originals if originals is not None else {}
        if key not in originals:
            originals[key] = await asyncio.to_thread(self._original, selector)
        source = originals[key]
        cache_key = digest([selector, source, stamp])
        if self.journal and cache_key in self.cache:
            self.journal.assert_unchanged(stamp)
            self.cache.move_to_end(cache_key)
            return self.cache[cache_key]
        scope = models.Filter(must=[models.FieldCondition(key=name, match=models.MatchValue(value=value))
                                    for name, value in selector.items()], must_not=[
            models.FieldCondition(key='node_role', match=models.MatchValue(value='navigation')),
            models.FieldCondition(key='evidence_admissible', match=models.MatchValue(value=False))])
        points, offset, chars = [], None, 0
        while True:
            page, offset = await self.backend.aclient.scroll(
                collection_name=self.backend.collection_name, scroll_filter=scope, offset=offset,
                limit=128, with_payload=True, with_vectors=False)
            points.extend(page)
            chars += sum(len(str((point.payload or {}).get('text') or '')) for point in page)
            if len(points) > 2048 or chars > 2_000_000:
                raise ValueError('Раздел слишком большой; выберите отдельный parent_id')
            if offset is None:
                break
        from proxy.services.document_task_store import native_request
        result = native_request('review_snapshot', dict(selector=selector, allowed=sorted(self.allowed),
            files=sorted(self.files), source=source,
            points=[{'id': str(point.id), 'payload': point.payload or {}} for point in points]))
        if self.journal:
            self.journal.assert_unchanged(stamp)
        if self.journal:
            self.cache[cache_key] = result
            while len(self.cache) > 8:
                self.cache.popitem(last=False)
        return result


def read_fragment(snapshot, *, fragment_index=0, offset=0, max_chars=4000):
    from proxy.services.document_task_store import native_request
    return native_request('review_fragment', dict(snapshot=snapshot, fragment_index=fragment_index,
                                                 offset=offset, max_chars=max_chars))
