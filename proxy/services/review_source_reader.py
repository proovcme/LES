"""Read explicitly selected indexed units and verify their original file bytes."""
from __future__ import annotations

import asyncio
import hashlib
from collections import OrderedDict
from pathlib import Path

from qdrant_client import models

from backend.index_replacement import ReplacementJournal
from proxy.services.document_task_store import digest


def validate_selector(selector, allowed):
    if (not isinstance(selector, dict) or set(selector) - {'dataset_id', 'file_name', 'parent_id'} or
            any(not isinstance(value, str) or not value or len(value) > 1000 for value in selector.values()) or
            not {'dataset_id', 'file_name'} <= set(selector) or selector['dataset_id'] not in allowed):
        raise ValueError('Выберите документ из текущих наборов: dataset_id, file_name и необязательный parent_id')
    relative = Path(selector['file_name'])
    if (relative.is_absolute() or '..' in relative.parts or relative.drive or
            Path(selector['dataset_id']).name != selector['dataset_id'] or selector['dataset_id'] in {'.', '..'}):
        raise ValueError('Нужен относительный путь документа из каталога')
    return dict(selector)


class ReviewSourceReader:
    def __init__(self, backend, dataset_ids, file_names=()):
        self.backend = backend
        self.allowed = frozenset(str(item) for item in dataset_ids)
        self.files = frozenset(file_names)
        self.journal = ReplacementJournal.for_adapter(backend) if hasattr(backend, 'content_dir') else None
        self.cache = OrderedDict()

    def _original(self, selector):
        row = self.backend.db.document_integrity_row(selector['dataset_id'], selector['file_name'])
        if not row or row['status'] != 'INDEXED' or not row.get('file_hash'):
            raise ValueError('Источник ещё не проиндексирован или его версия неизвестна')
        if row.get('source_path'):
            path = Path(row['source_path'])
        else:
            root = (Path(self.backend.content_dir) / selector['dataset_id']).resolve()
            path = (root / selector['file_name']).resolve()
            if not path.is_relative_to(root):
                raise ValueError('Источник находится вне каталога набора')
        checksum = hashlib.sha256()
        try:
            with path.open('rb') as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b''):
                    checksum.update(block)
        except OSError as error:
            raise ValueError('Оригинал недоступен; проверьте файл и права чтения') from error
        if checksum.hexdigest() != row['file_hash']:
            raise ValueError('Файл изменился; обновите индекс перед проверкой')
        return {'sha256': checksum.hexdigest(), 'document_id': row['id']}

    async def read(self, selector, *, originals=None):
        selector = validate_selector(selector, self.allowed)
        if self.files and selector['file_name'] not in self.files:
            raise ValueError('Документ не входит в выбранную область файлов')
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
        fragments = []
        for point in points:
            payload = point.payload or {}
            if (any(str(payload.get(key) or '') != value for key, value in selector.items()) or
                    payload.get('node_role') == 'navigation' or payload.get('evidence_admissible') is False):
                raise ValueError('Фрагмент не принадлежит выбранному разделу')
            text = str(payload.get('text') or '')
            if text:
                fragments.append({'id': str(point.id), 'text': text,
                                  'locator': {key: payload[key] for key in
                                              ('page', 'source_page', 'source_ref', 'chunk_ord', 'parent_id', 'doc_id')
                                              if payload.get(key) is not None}})
        fragments.sort(key=lambda item: (int(item['locator'].get('chunk_ord') or 0), item['id']))
        if not fragments or len({item['id'] for item in fragments}) != len(fragments):
            raise ValueError('В выбранном разделе нет однозначных индексированных фрагментов')
        if self.journal:
            self.journal.assert_unchanged(stamp)
        result = {'selector': selector, 'source_sha256': source['sha256'], 'fragments': fragments,
                'fingerprint': digest([selector, source, fragments]),
                'coverage_scope': 'indexed_text_only', 'original_visual_review': False}
        if self.journal:
            self.cache[cache_key] = result
            while len(self.cache) > 8:
                self.cache.popitem(last=False)
        return result


def read_fragment(snapshot, *, fragment_index=0, offset=0, max_chars=4000):
    if (type(fragment_index) is not int or type(offset) is not int or offset < 0 or
            not 0 <= fragment_index < len(snapshot['fragments']) or type(max_chars) is not int or
            not 200 <= max_chars <= 4000):
        raise ValueError('Некорректная позиция чтения раздела')
    fragment = snapshot['fragments'][fragment_index]
    if offset >= len(fragment['text']):
        raise ValueError('Смещение за пределами фрагмента')
    stop = min(offset + max_chars, len(fragment['text']))
    next_cursor = ({'fragment_index': fragment_index, 'offset': stop} if stop < len(fragment['text']) else
                   {'fragment_index': fragment_index + 1, 'offset': 0}
                   if fragment_index + 1 < len(snapshot['fragments']) else None)
    return {'fragment_id': fragment['id'], 'locator': fragment['locator'], 'offset': offset, 'stop': stop,
            'text': fragment['text'][offset:stop], 'next_cursor': next_cursor,
            'source_fingerprint': snapshot['fingerprint'], 'source_sha256': snapshot['source_sha256'],
            'selector': snapshot['selector'], 'fragment_count': len(snapshot['fragments']),
            'coverage_scope': snapshot['coverage_scope'], 'original_visual_review': False}
