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


def test_native_ledger_matches_frozen_receipts_and_rejects_invalid_counts():
    import random
    from tests.oracles import execution_receipts_780 as old
    rng = random.Random(781)
    for index in range(200):
        parts = {name:rng.randrange(0,5) for name in ['pending','reviewed','needs_review','excluded','needs_recheck']}
        state = dict(task_id='Тест 日本語',revision=index,total=sum(parts.values()),**parts,
                     blocked_units=rng.randrange(0,2),verification_unavailable=rng.choice([False,True,0,'yes']))
        for kind in ['table_document','document_review']:
            assert ledger_receipt(kind,state) == old.ledger_receipt(kind,state)
    for value in [True,-1,1.0,'1']:
        with pytest.raises(ValueError):ledger_receipt('document_review',dict(task_id='t',revision=0,total=value))


def test_mixed_success_and_failure_notices_keep_all_facts_and_deduplicate_text():
    from proxy.services.execution_receipt_service import local_file_receipt
    saved = local_file_receipt(dict(status='ok',operation='edit',result=dict(
        path='workspaces/1/notes/План.md',persisted=True,changed=True,revision='real-revision',history_id='history')))
    failed = local_file_receipt(dict(status='error',operation='edit',error={'code':'AGENT_CONFLICT'}))
    for tasks in [[saved,failed,failed],[failed,failed,saved]]:
        result = summarize(tasks)
        assert result['status']=='incomplete' and result['accepted'] is False
        assert result['tasks']==tasks  # No failure is hidden or rewritten into success.
        assert result['notice'].startswith(saved['notice'])
        assert result['notice'].count(failed['notice'])==1
        assert execution_incomplete({'execution':result})
    forged = local_file_receipt(dict(status='ok',operation='save',answer='Всё выполнено.',result={'revision':'invented'}))
    assert forged['status']=='incomplete'
    assert summarize([None]) is None


def test_receipt_boundary_does_not_copy_document_or_file_text():
    from proxy.services.execution_receipt_service import local_file_receipt
    large = 'x' * (9 * 1024 * 1024)  # Beyond the ABI limit, irrelevant to a commit receipt.
    receipt = ledger_receipt('document_review',dict(task_id='t',revision=1,total=1,reviewed=1,units=large))
    assert receipt['status']=='recorded' and 'units' not in receipt
    receipt = local_file_receipt(dict(status='ok',operation='save',result=dict(
        path='USER.md',persisted=True,revision='native-revision',text=large)))
    assert receipt['status']=='recorded' and 'text' not in receipt
