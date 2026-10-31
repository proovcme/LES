import asyncio
import json
import sqlite3
from types import SimpleNamespace

import pytest

from proxy.services import chat_persistence_service as history
from proxy.services.execution_receipt_service import ledger_receipt, summarize, execution_incomplete
from proxy.services.review_task_store import ReviewTaskStore
from proxy.services.document_review_tool import DocumentReviewTool
from sovushka.answer_render import tool_execution_notice


def review(tmp_path):
    store = ReviewTaskStore(tmp_path / 'review.db')
    selector = dict(dataset_id='D', file_name='Пример с пробелами.txt')
    task_id = store.create('owner', 'Проверить раздел', [selector])
    snapshot = dict(fingerprint='v1', source_sha256='sha', selector=selector,
                    fragments=[dict(id='point', text='Условие: 12 м.', locator={'page': 1})])
    async def read(*_args, **_kwargs):
        return snapshot
    tool = DocumentReviewTool(store, SimpleNamespace(journal=None, read=read), 'owner', 'Проверить раздел')
    return store, tool, task_id, snapshot


def test_model_completion_claim_is_not_a_committed_action(tmp_path):
    store, tool, task, snapshot = review(tmp_path)
    receipt = asyncio.run(tool.completion_receipt())
    execution = summarize([receipt])
    assert execution_incomplete({'execution': execution})
    assert receipt['counts']['pending'] == 1 and receipt['saved'] == 0 and not receipt['accepted']
    assert '0 из 1' in tool_execution_notice({'execution': execution})
    assert store.history('owner', task)['events'] == []
    unit = store.task('owner', task)['units'][0]['unit_id']
    store.remember_read('owner', task, unit, snapshot, 'point', 0, len(snapshot['fragments'][0]['text']))
    store.record('owner', task, request_id='save-one', expected_revision=0, unit_id=unit, snapshot=snapshot,
                 source_fingerprint='v1', status='reviewed', note='Вывод модели',
                 evidence=[dict(fragment_id='point', quote='12 м')], dependencies=[])
    saved = asyncio.run(tool.completion_receipt())
    assert saved['status'] == 'recorded' and saved['revision'] == 1
    assert not saved['accepted'] and saved['saved'] == 1
    assert not execution_incomplete({'execution': summarize([saved])})


def test_changed_source_cannot_keep_a_completed_receipt(tmp_path):
    store, tool, task, snapshot = review(tmp_path)
    unit = store.task('owner', task)['units'][0]['unit_id']
    store.remember_read('owner', task, unit, snapshot, 'point', 0, len(snapshot['fragments'][0]['text']))
    store.record('owner', task, request_id='save-one', expected_revision=0, unit_id=unit, snapshot=snapshot,
                 source_fingerprint='v1', status='reviewed', note='Вывод модели',
                 evidence=[dict(fragment_id='point', quote='12 м')], dependencies=[])
    assert asyncio.run(tool.completion_receipt())['status'] == 'recorded'
    snapshot['fingerprint'] = 'v2'
    receipt = asyncio.run(tool.completion_receipt())
    assert receipt['status'] == 'needs_recheck'
    assert summarize([receipt])['status'] == 'incomplete'


def test_ledger_counts_are_validated_and_empty_is_not_complete():
    state = dict(task_id='task', revision=0, total=0)
    assert ledger_receipt('document_review', state)['status'] == 'incomplete'
    with pytest.raises(ValueError):
        ledger_receipt('document_review', state | dict(total=1, reviewed=2))
    assert summarize([]) is None


def test_explicit_failure_survives_every_history_schema_read(tmp_path, monkeypatch):
    path = tmp_path / 'history.db'
    monkeypatch.setattr(history, 'rag_meta_db_path', lambda: str(path))
    monkeypatch.setattr(history, 'update_chat_profile', lambda *_args, **_kwargs: None)
    receipt = ledger_receipt('document_review', dict(task_id='task', revision=0, total=1, pending=1))
    trace = {'execution': summarize([receipt])}
    history_id = history.save_chat_history(question='Проверь', answer='Модель заявляет: всё сохранено.',
        sources=[], crag_status='VERIFIED', latency_sec=1, tokens=1, session_id='demo', retrieval_trace=trace, success=1)
    with sqlite3.connect(path) as db:
        for _ in range(3):
            history.ensure_chat_history_schema(db)
        row = db.execute('SELECT success, retrieval_trace_json FROM chat_history WHERE id=?', (history_id,)).fetchone()
    assert row[0] == 0
    assert json.loads(row[1])['execution']['tasks'][0]['counts']['pending'] == 1


def test_legacy_backfill_runs_only_when_adding_the_success_column():
    with sqlite3.connect(':memory:') as db:
        db.execute('CREATE TABLE chat_history (id INTEGER PRIMARY KEY, question TEXT, answer TEXT, crag_status TEXT)')
        db.execute("INSERT INTO chat_history VALUES (1, 'q', 'a', 'VERIFIED')")
        history.ensure_chat_history_schema(db)
        assert db.execute('SELECT success FROM chat_history').fetchone()[0] == 1
        db.execute('UPDATE chat_history SET success=0')
        history.ensure_chat_history_schema(db)
        assert db.execute('SELECT success FROM chat_history').fetchone()[0] == 0
