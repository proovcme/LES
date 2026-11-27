import json
import sqlite3

import pytest

from proxy.services import chat_persistence_service as history
from proxy.services.execution_receipt_service import ledger_receipt, summarize


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
    for _ in range(3):
        history.ensure_chat_history_schema(path)
    with sqlite3.connect(path) as db:
        row = db.execute('SELECT success, retrieval_trace_json FROM chat_history WHERE id=?', (history_id,)).fetchone()
    assert row[0] == 0
    assert json.loads(row[1])['execution']['tasks'][0]['counts']['pending'] == 1


def test_legacy_backfill_runs_only_when_adding_the_success_column(tmp_path):
    path = tmp_path / 'legacy.sqlite'
    with sqlite3.connect(path) as db:
        db.execute('CREATE TABLE chat_history (id INTEGER PRIMARY KEY, question TEXT, answer TEXT, crag_status TEXT)')
        db.execute("INSERT INTO chat_history VALUES (1, 'q', 'a', 'VERIFIED')")
    history.ensure_chat_history_schema(path)
    with sqlite3.connect(path) as db:
        assert db.execute('SELECT success FROM chat_history').fetchone()[0] == 1
        db.execute('UPDATE chat_history SET success=0')
    history.ensure_chat_history_schema(path)
    with sqlite3.connect(path) as db:
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


def test_receipt_boundary_does_not_copy_document_or_file_text():
    from proxy.services.execution_receipt_service import local_file_receipt
    large = 'x' * (9 * 1024 * 1024)  # Beyond the ABI limit, irrelevant to a commit receipt.
    receipt = ledger_receipt('document_review',dict(task_id='t',revision=1,total=1,reviewed=1,units=large))
    assert receipt['status']=='recorded' and 'units' not in receipt
    receipt = local_file_receipt(dict(status='ok',operation='save',result=dict(
        path='USER.md',persisted=True,revision='native-revision',text=large)))
    assert receipt['status']=='recorded' and 'text' not in receipt
