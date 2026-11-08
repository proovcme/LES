"""Classic transport only. Immutable rows, transactions and decisions belong to Rust."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory

from backend import native_core
from proxy.services.tabular_document_service import encode


def digest(value):
    return hashlib.sha256(encode(value).encode('utf-8')).hexdigest()


def native_request(action, arguments, database=None):
    request = {'schema': 'les.document-tasks.v1', 'action': action, 'arguments': arguments}
    if database is not None:
        request['database'] = str(Path(database).resolve())
    # Large snapshots cross the ABI through owned, short-lived files.
    with TemporaryDirectory(prefix='les-table-transport-') as directory:
        source, target = Path(directory)/'request.json', Path(directory)/'response.json'
        with source.open('w', encoding='utf-8') as stream:
            json.dump(request, stream, ensure_ascii=False, allow_nan=False, separators=(',', ':'))
        result = native_core.required('document_tasks', request={
            'input_file': str(source), 'output_file': str(target)})
        if result.get('status') != 'completed':
            raise ValueError(result.get('detail') or result.get('code') or 'Не удалось открыть задание')
        with target.open(encoding='utf-8') as stream:
            return json.load(stream)


class DocumentTaskStore:
    def __init__(self, path: Path):
        self.path = Path(path).resolve()
        self._call('init')

    def _call(self, action, owner='', task_id='', **arguments):
        return native_request(action, {'owner': owner, 'task_id': task_id, **arguments}, self.path)

    def open(self, owner, question, document, *, scope_key=''):
        return self._call('open', owner, question=question, document=document, scope_key=scope_key)

    def latest(self, scope_key):
        return self._call('latest', scope_key=scope_key)

    def activate(self, owner, task_id):
        return self._call('activate', owner, task_id)

    def new_question(self, owner, task_id, question):
        return self._call('new_question', owner, task_id, question=question)

    def snapshot(self, owner, task_id):
        return self._call('snapshot', owner, task_id)

    def status(self, owner, task_id):
        return self._call('status', owner, task_id)

    def matches_question(self, owner, task_id, question):
        return self._call('matches_question', owner, task_id, question=question)

    def recent(self, owner):
        return self._call('recent', owner)

    def next_packet(self, owner, task_id, *, max_bytes=6000, max_rows=6):
        return self._call('next_packet', owner, task_id, max_bytes=max_bytes, max_rows=max_rows)

    def commit(self, owner, task_id, packet_id, decisions):
        return self._call('commit', owner, task_id, packet_id=packet_id, decisions=decisions)

    def results(self, owner, task_id, *, offset=0, max_bytes=9000):
        return self._call('results', owner, task_id, offset=offset, max_bytes=max_bytes)
