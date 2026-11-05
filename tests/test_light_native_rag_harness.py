"""Frozen-779 parity plus transactional failures through the real native ABI."""
from dataclasses import asdict
from pathlib import Path
from types import SimpleNamespace
import json
import os
import random
import sqlite3
import subprocess
import sys

import pytest
from backend import bm25_store as store, native_core
from backend.interface import EmbeddingContractError
from proxy.services import context_governor_service as context
from tests.oracles import bm25_store_779 as old_store, context_governor_779 as old_context
from tests.test_context_governor_service import _preset


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build core and set LES_CORE_LIBRARY for native acceptance')


class Journal:
    def __init__(self, directory):
        self.directory = directory
        directory.mkdir()
        self.revision = 'one'

    def read_stamp(self):
        return self.revision

    def assert_unchanged(self, stamp):
        if stamp != self.revision:
            raise EmbeddingContractError('INDEX_SPARSE_STALE')


def _points():
    rng = random.Random(780)
    vocabulary = ['кабель', 'оборудования', 'PE', 'РЕ', 'СП 256.1325800', 'ЩР-2', 'BB_63', '日本語 🌲']
    return [SimpleNamespace(id=i if i % 2 else f'номер-{i}', payload=dict(
        text=' '.join(rng.choices(vocabulary, k=rng.randrange(0, 40))),
        dataset_id=f'd{i%3}', file_name=f'f{i%7}', node_role='evidence' if i%2 else 'navigation',
        ancestor_ids=[f'parent-{i%4}', 'root'])) for i in range(513)]


def _write(journal, points):
    with store.writer(journal) as handle:
        store.reset(handle)
        store.put(handle, points)
        store.commit(handle, journal.revision, len(points))


def _tables(journal):
    with sqlite3.connect(journal.directory/'bm25.sqlite') as db:
        return {table: db.execute(f'SELECT * FROM {table} ORDER BY {order}').fetchall()
                for table, order in [('docs','id'),('postings','term,doc'),('frequencies','term'),
                                     ('ancestors','ancestor,doc'),('corpus','id')]}


def test_postings_scores_scopes_and_incremental_replacements_match_779(tmp_path):
    current, old = Journal(tmp_path/'native'), Journal(tmp_path/'old')
    points = _points()
    _write(current, points)
    with old_store.connect(old, create=True) as db, db:
        old_store.put(db, points)
        db.execute('UPDATE corpus SET revision=?', (old.revision,))
    assert _tables(current) == _tables(old)
    for query in ['кабель PE', 'ЩР-2', 'РЕ', 'сп 256.1325800', 'несуществующий', 'кабель кабель']:
        for filters in [{}, dict(dataset_ids=['d1']), dict(doc_filter=['f3'], node_roles=['evidence']),
                        dict(ancestor_ids=['parent-2']), dict(ancestor_ids=['absent'])]:
            actual = store.search(current, query, limit=17, **filters)
            expected = old_store.search(old, query, limit=17, **filters)
            assert [i for i,_ in actual] == [i for i,_ in expected]
            assert [s for _,s in actual] == pytest.approx([s for _,s in expected], rel=1e-13)
    for scope in [dict(dataset='d1', file='f3'), dict(ids=[1,'номер-2']), dict(datasets=['d2'])]:
        with store.writer(current) as handle:
            store.clear_scope(handle, scope)
            store.put(handle, [points[1]])
            # Independent oracle supplies expected global corpus count.
            with old_store.connect(old) as db, db:
                old_store.clear_scope(db, scope)
                old_store.put(db, [points[1]])
                count = db.execute('SELECT n FROM corpus').fetchone()[0]
                db.execute('DELETE FROM frequencies WHERE df=0')
            store.commit(handle, current.revision, count)
        assert _tables(current) == _tables(old)


def test_failed_count_batch_and_missing_core_never_publish_partial_data(tmp_path, monkeypatch):
    journal = Journal(tmp_path/'index')
    _write(journal, _points()[:20])
    before = _tables(journal)
    with pytest.raises(EmbeddingContractError, match='INDEX_SPARSE_STALE'):
        with store.writer(journal) as handle:
            store.reset(handle)
            store.commit(handle, 'bad', 1)
    assert _tables(journal) == before
    with pytest.raises(EmbeddingContractError, match='INDEX_SPARSE_REQUEST_INVALID'):
        with store.writer(journal) as handle:
            store.reset(handle)
            store._call('put', handle=handle, points=[dict(id='1',terms={'bad':-1})])
    assert _tables(journal) == before
    journal.revision = 'stale'
    with pytest.raises(EmbeddingContractError, match='INDEX_SPARSE_STALE'):
        store.search(journal, 'кабель')
    monkeypatch.setenv('LES_CORE_LIBRARY', str(tmp_path/'absent.dll'))
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_UNAVAILABLE'):
        store.metadata(journal)
    with pytest.raises(native_core.NativeCoreError, match='CORE_ASSET_UNAVAILABLE'):
        context.ContextGovernor(_preset()).pack([])


def test_writer_limit_is_bounded_and_all_handles_can_be_released(tmp_path):
    handles = []
    try:
        for i in range(8):
            j = Journal(tmp_path/f'writer-{i}')
            handles.append(store._call('begin', database=store._database(j))['handle'])
        ninth = Journal(tmp_path/'ninth')
        with pytest.raises(EmbeddingContractError, match='INDEX_SPARSE_OWNER_BUSY'):
            store._call('begin', database=store._database(ninth))
    finally:
        for handle in handles:
            store._call('rollback', handle=handle)
    _write(ninth, _points()[:1])
    assert store.metadata(ninth)[0] == 1


def test_legacy_scalar_metadata_is_preserved_and_invalid_shapes_cannot_silently_disappear(tmp_path):
    current, old = Journal(tmp_path/'current'), Journal(tmp_path/'old')
    points = [SimpleNamespace(id=1,payload=dict(text='кабель',dataset_id=42,file_name=True,
                                             node_role=1.5,ancestor_ids=['root']))]
    _write(current,points)
    with old_store.connect(old,create=True) as db, db:
        old_store.put(db,points)
        db.execute('UPDATE corpus SET revision=?',(old.revision,))
    assert _tables(current) == _tables(old)
    before = _tables(current)
    for key, value in [('dataset_id',{'bad':'shape'}),('ancestor_ids','not-a-list')]:
        bad = SimpleNamespace(id=1,payload={**points[0].payload,key:value})
        with pytest.raises(EmbeddingContractError,match='INDEX_SPARSE_REQUEST_INVALID'):
            _write(current,[bad])
        assert _tables(current) == before


def test_real_writer_process_exit_rolls_back_and_reader_sees_committed_snapshot(tmp_path):
    journal = Journal(tmp_path/'index')
    _write(journal, _points()[:20])
    before = _tables(journal)
    with store.writer(journal) as handle:
        store.reset(handle)
        assert store.metadata(journal)[0] == 20  # WAL reader does not see reset.
    script = '''
import os,sys
from pathlib import Path
from types import SimpleNamespace
from backend import bm25_store as store
j=SimpleNamespace(directory=Path(sys.argv[1]))
with store.writer(j) as handle:
    store.reset(handle)
    os._exit(27)
'''
    result = subprocess.run([sys.executable,'-c',script,str(journal.directory)], timeout=20)
    assert result.returncode == 27
    assert _tables(journal) == before
    _write(journal, _points()[:21])
    assert store.metadata(journal)[0] == 21


def test_context_randomized_parity_complete_objects_overflow_and_cursors():
    rng = random.Random(780)
    for trial in range(250):
        preset = _preset(limit=rng.randrange(0,1800), generation=rng.randrange(0,100), safety=30)
        candidates = []
        old_candidates = []
        for i in range(rng.randrange(1,25)):
            kind = rng.choice(list(context.ContextKind))
            objects = []
            for j in range(rng.randrange(0,5)):
                payload = ('Русский\t日本語🌲\r\n'*rng.randrange(0,20) if kind != context.ContextKind.NATIVE_TOOL_EXCHANGE
                           else [dict(role='assistant',content=None,tool_calls=[dict(id=f'call-{i}-{j}')]),
                                 dict(role='tool',tool_call_id=f'call-{i}-{j}',content='{"ok":true}')])
                if kind == context.ContextKind.DIALOGUE:
                    payload = dict(role=rng.choice(['user','assistant','other']),content=payload)
                objects.append(context.ContextObject(f'{i}-{j}',payload))
            required = rng.random()<.15
            candidates.append(context.ContextCandidate(kind,tuple(objects),required))
            old_candidates.append(old_context.ContextCandidate(old_context.ContextKind(kind.value),
                tuple(old_context.ContextObject(o.object_id,o.payload) for o in objects),required))
        estimator = len if trial % 2 else context.conservative_token_estimate
        try:
            expected = old_context.ContextGovernor(preset,estimate_tokens=estimator).pack(old_candidates)
        except old_context.ContextRequiredSectionOverflow as error:
            with pytest.raises(context.ContextRequiredSectionOverflow) as actual:
                context.ContextGovernor(preset,estimate_tokens=estimator).pack(candidates)
            assert (actual.value.budget,actual.value.required_tokens,actual.value.object_ids) == (
                error.budget,error.required_tokens,error.object_ids)
        else:
            actual = context.ContextGovernor(preset,estimate_tokens=estimator).pack(candidates)
            assert asdict(actual) == asdict(expected)
            assert actual.as_messages() == expected.as_messages()
            assert actual.as_messages(request_last=True) == expected.as_messages(request_last=True)


def test_rrf_ties_duplicates_typed_ids_and_constants_match_existing_order():
    from backend.bm25_hybrid import fuse
    from qdrant_client.models import ScoredPoint
    rng = random.Random(780)
    for _ in range(100):
        channels = [[ScoredPoint(id=rng.choice([1,2,3,'1','2','3']),version=0,
            score=rng.random(),payload={'branch':branch}) for _ in range(rng.randrange(0,15))]
            for branch in range(2)]
        constant = rng.choice([.1,2.,60.])
        limit = rng.randrange(-3,15)
        scores, first = {}, {}
        for channel in channels:
            for rank, point in enumerate(channel):
                first.setdefault(point.id,point)
                scores[point.id] = scores.get(point.id,0.)+1./(constant+rank)
        expected = [first[key].model_copy(update={'score':score}) for key,score in
                    sorted(scores.items(),key=lambda row:row[1],reverse=True)[:limit]]
        assert fuse(*channels,limit,rank_constant=constant) == expected
