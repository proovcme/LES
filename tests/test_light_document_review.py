"""Resumable review: actual provenance, delivery, stale dependencies and boundaries."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
from types import SimpleNamespace

import pytest

from proxy.services.review_task_store import ReviewTaskStore
from proxy.services.review_source_reader import ReviewSourceReader, read_fragment
from proxy.services.document_review_tool import DocumentReviewTool
from proxy.services.chat_evidence_context import govern_inference_messages
from proxy.services.model_execution_preset_service import _FACTORY_9B
from proxy.services.bound_document_tools import document_tool_context


def snapshot(text='Условие: не менее 12 м.', revision='v1'):
    return {'fingerprint': revision, 'source_sha256': 'sha',
            'selector': {'dataset_id': 'D', 'file_name': 'source.txt'},
            'fragments': [{'id': 'point', 'text': text, 'locator': {'page': 2}}],
            'coverage_scope': 'indexed_text_only'}


def setup_store(tmp_path, count=2):
    store = ReviewTaskStore(tmp_path/'tasks.db')
    selectors = [{'dataset_id': 'D', 'file_name': f'file{index}.pdf', 'parent_id': 's1'} for index in range(count)]
    task = store.create('owner', 'Проверить выбранные разделы', selectors)
    units = [item['unit_id'] for item in store.task('owner', task)['units']]
    return store, task, units


def record(store, task, unit, revision, *, snap=None, dependencies=(), request_id=None, **changes):
    snap = snap or snapshot()
    snap = {**snap, 'selector': next(item['selector'] for item in store.task('owner', task)['units'] if item['unit_id'] == unit)}
    return store.record('owner', task, request_id=request_id or f'request{revision}', expected_revision=revision,
        unit_id=unit, snapshot=snap, source_fingerprint=snap['fingerprint'], status='reviewed', note='Наблюдение модели',
        evidence=[{'fragment_id': 'point', 'quote': '12 м'}], dependencies=list(dependencies), **changes)


def deliver(store, task, unit, snap=None, start=0, stop=None):
    snap = snap or snapshot()
    snap = {**snap, 'selector': next(item['selector'] for item in store.task('owner', task)['units'] if item['unit_id'] == unit)}
    store.remember_read('owner', task, unit, snap, 'point', start, len(snap['fragments'][0]['text']) if stop is None else stop)


def test_resume_exact_inventory_history_and_transitive_staleness(tmp_path):
    store, task, (a, b, c) = setup_store(tmp_path, 3)
    for unit in (a, b, c):
        deliver(store, task, unit)
    record(store, task, a, 0)
    record(store, task, b, 1, dependencies=[a])
    record(store, task, c, 2, dependencies=[b])
    store = ReviewTaskStore(store.path)
    status = store.status('owner', task, dict.fromkeys((a, b, c), 'v1'))
    assert status['coverage_complete'] and status['reviewed'] == 3
    assert status['accepted'] is False and status['scope'] == 'explicit_inventory_only'
    stale = store.status('owner', task, {a: 'changed', b: 'v1', c: 'v1'})
    assert stale['needs_recheck'] == 3 and not stale['coverage_complete'] and stale['next_unit_id'] == a
    # Re-evaluating the same source changes the result revision; dependants stay stale.
    record(store, task, a, 3)
    stale = store.status('owner', task, dict.fromkeys((a, b, c), 'v1'))
    assert stale['needs_recheck'] == 2 and stale['reviewed'] == 1
    assert len(store.history('owner', task)['events']) == 4
    assert store.create('owner', 'Проверить выбранные разделы',
                        [unit['selector'] for unit in store.task('owner', task)['units']]) == task
    with pytest.raises(ValueError):
        store.task('other-chat', task)


def test_missing_source_is_stale_and_rebase_preserves_old_conclusion(tmp_path):
    store, task, (unit,) = setup_store(tmp_path, 1)
    deliver(store, task, unit)
    record(store, task, unit, 0)
    assert store.status('owner', task, {})['needs_recheck'] == 1
    changed = snapshot(revision='v2')
    with pytest.raises(ValueError, match='прочитанной'):
        record(store, task, unit, 1, snap=changed)
    deliver(store, task, unit, changed)
    record(store, task, unit, 1, snap=changed)
    assert store.status('owner', task, {unit: 'v2'})['coverage_complete']
    assert [event['fingerprint'] for event in store.history('owner', task)['events']] == ['v1', 'v2']


def test_partial_or_undelivered_reads_and_invented_quotes_cannot_complete(tmp_path):
    store, task, (unit,) = setup_store(tmp_path, 1)
    with pytest.raises(ValueError, match='прочитанной'):
        record(store, task, unit, 0)
    deliver(store, task, unit, start=9)
    with pytest.raises(ValueError, match='не полностью'):
        record(store, task, unit, 0)
    deliver(store, task, unit, stop=9)
    with pytest.raises(ValueError, match='прочитанной'):
        store.record('owner', task, request_id='lie', expected_revision=0, unit_id=unit,
            snapshot={**snapshot(), 'selector': store.task('owner', task)['units'][0]['selector']},
            source_fingerprint='v1', status='reviewed', note='Вывод',
            evidence=[{'fragment_id': 'point', 'quote': '120 м'}], dependencies=[])
    assert record(store, task, unit, 0) == 1
    assert record(store, task, unit, 0) == 1  # Network redelivery, including old expected_revision.
    assert store.task('owner', task)['revision'] == 1
    with pytest.raises(ValueError, match='другого'):
        record(store, task, unit, 0, dependencies=[unit])


def test_concurrent_writers_compare_and_swap(tmp_path):
    store, task, (a, b) = setup_store(tmp_path, 2)
    for unit in (a, b):
        deliver(store, task, unit)
    def save(unit):
        try:
            return record(ReviewTaskStore(store.path), task, unit, 0, request_id=unit)
        except ValueError:
            return 'conflict'
    with ThreadPoolExecutor(2) as pool:
        results = list(pool.map(save, (a, b)))
    assert sorted(map(str, results)) == ['1', 'conflict']
    assert len(store.history('owner', task)['events']) == 1


def test_reconverging_dependencies_and_cycles_are_bounded(tmp_path):
    store, task, units = setup_store(tmp_path, 24)
    for index, unit in enumerate(units):
        deliver(store, task, unit)
        record(store, task, unit, index, dependencies=units[max(0, index-16):index])
    state = store.status('owner', task, dict.fromkeys(units, 'v1'))
    assert state['reviewed'] == 24
    # Creating a cycle through result revisions makes affected conclusions stale.
    record(store, task, units[0], 24, dependencies=[units[-1]])
    assert store.status('owner', task, dict.fromkeys(units, 'v1'))['needs_recheck'] == 24


def test_store_rejects_a_different_unit_snapshot(tmp_path):
    store, task, (unit,) = setup_store(tmp_path, 1)
    with pytest.raises(ValueError, match='список'):
        store.remember_read('owner', task, unit, snapshot(), 'point', 0, 100)
    with pytest.raises(ValueError, match='принадлежит'):
        store.record('owner', task, request_id='foreign', expected_revision=0, unit_id=unit,
            snapshot=snapshot(), source_fingerprint='v1', status='reviewed', note='Наблюдение',
            evidence=[{'fragment_id': 'point', 'quote': '12 м'}], dependencies=[])


def test_exact_owned_reference_and_retry_resolve_without_topic_guessing(tmp_path):
    store, task, _ = setup_store(tmp_path, 1)
    assert store.resolve_reference('owner', 'Продолжи task_id '+task) == task
    assert store.resolve_reference('owner', 'Проверить выбранные разделы') == task
    assert store.resolve_reference('owner', 'Что было в документе?') is None
    assert store.resolve_reference('foreign', 'Продолжи '+task) is None
    other = store.create('owner', 'Другая цель', [{'dataset_id': 'D', 'file_name': 'other.txt'}])
    assert store.resolve_reference('owner', task+' и '+other) is None
    store.create('owner', 'Проверить выбранные разделы', [{'dataset_id': 'D', 'file_name': 'other.txt'}])
    assert store.resolve_reference('owner', 'Проверить выбранные разделы') is None


def test_resumed_context_and_notice_use_ledger_not_model_completion_claim(tmp_path):
    adapter, path, _, _ = backend(tmp_path)
    store = ReviewTaskStore(tmp_path/'tasks.db')
    task = store.create('owner', 'Проверить', [{'dataset_id': 'D', 'file_name': 'source.txt'}])
    resumed = DocumentReviewTool(store, ReviewSourceReader(adapter, ['D']), 'owner', 'Продолжи task_id '+task)
    asyncio.run(resumed.prepare_context())
    assert resumed.context()['document_review']['pending'] == 1
    assert '0 из 1' in asyncio.run(resumed.coverage_notice())
    # No model claim is persisted or translated into a successful review event.
    assert store.history('owner', task)['events'] == []
    unrelated = DocumentReviewTool(store, resumed.reader, 'owner', 'Другой вопрос')
    assert asyncio.run(unrelated.coverage_notice()) == ''


def backend(tmp_path):
    path = tmp_path/'original.txt'
    path.write_text('Проверяемые данные', encoding='utf-8')
    row = {'id': 'doc', 'file_name': 'source.txt', 'status': 'INDEXED', 'source_path': str(path),
           'file_hash': hashlib.sha256(path.read_bytes()).hexdigest()}
    point = SimpleNamespace(id='point', payload={'text': 'Требуется 12 м.', 'dataset_id': 'D',
        'file_name': 'source.txt', 'parent_id': 's1', 'page': 2, 'chunk_ord': 0, 'doc_id': 'doc'})
    class Client:
        async def scroll(self, **kwargs):
            return [point], None
    return SimpleNamespace(db=SimpleNamespace(document_integrity_row=lambda dataset, file: dict(row) if file == row['file_name'] else None),
                           aclient=Client(), collection_name='test'), path, row, point


def test_source_reader_checks_real_bytes_even_when_stat_unchanged(tmp_path):
    adapter, path, row, point = backend(tmp_path)
    reader = ReviewSourceReader(adapter, ['D'])
    selector = {'dataset_id': 'D', 'file_name': 'source.txt', 'parent_id': 's1'}
    first = asyncio.run(reader.read(selector))
    saved_stat = path.stat()
    data = path.read_bytes()
    path.write_bytes(b'X' + data[1:])
    os.utime(path, ns=(saved_stat.st_atime_ns, saved_stat.st_mtime_ns))
    with pytest.raises(ValueError, match='обновите индекс'):
        asyncio.run(reader.read(selector))
    row['file_hash'] = hashlib.sha256(path.read_bytes()).hexdigest()
    second = asyncio.run(reader.read(selector))
    assert second['fingerprint'] != first['fingerprint']
    point.payload['text'] += ' Изменён индекс.'
    assert asyncio.run(reader.read(selector))['fingerprint'] != second['fingerprint']
    with pytest.raises(ValueError):
        asyncio.run(reader.read({**selector, 'dataset_id': 'other'}))
    with pytest.raises(ValueError):
        asyncio.run(ReviewSourceReader(adapter, ['D'], ['other.txt']).read(selector))
    with pytest.raises(ValueError):
        asyncio.run(reader.read({**selector, 'file_name': '../escape.txt'}))


def test_original_lookup_uses_exact_catalog_provenance_and_rejects_duplicates(tmp_path, monkeypatch):
    from backend.document_catalog import MetaDB
    adapter, path, _, _ = backend(tmp_path)
    db = MetaDB(str(tmp_path/'catalog.db'))
    own, _, _ = db.add_document('D', 'source.txt', source_path=str(path))
    db.add_document('other', 'source.txt', source_path=str(path))
    with db._get_conn() as connection:
        connection.execute("UPDATE documents SET status='INDEXED',file_hash=? WHERE id=?",
                           (hashlib.sha256(path.read_bytes()).hexdigest(), own))
    assert db.document_integrity_row('D', 'missing') is None
    assert db.document_integrity_row('D', 'source.txt')['id'] == own
    adapter.db = db
    def no_dataset_scan(*args):
        raise AssertionError('Reading one source must not materialize the entire dataset catalog')
    monkeypatch.setattr(db, 'dataset_integrity_rows', no_dataset_scan)
    reader = ReviewSourceReader(adapter, ['D'])
    asyncio.run(reader.read({'dataset_id': 'D', 'file_name': 'source.txt'}))
    with db._get_conn() as connection:
        connection.execute("INSERT INTO documents(id,dataset_id,file_name,status) VALUES('duplicate','D','source.txt','INDEXED')")
    with pytest.raises(ValueError, match='несколько версий'):
        asyncio.run(reader.read({'dataset_id': 'D', 'file_name': 'source.txt'}))


def test_bounded_fragment_cursors_never_skip_unicode():
    snap = snapshot('А😀Б' * 500)
    cursor, pieces = {}, []
    while True:
        page = read_fragment(snap, **cursor, max_chars=203)
        pieces.append(page['text'])
        if page['next_cursor'] is None:
            break
        cursor = page['next_cursor']
    assert ''.join(pieces) == snap['fragments'][0]['text']
    assert max(map(len, pieces)) == 203
    with pytest.raises(ValueError):
        read_fragment(snap, offset=True)


def test_index_generation_cache_reuses_only_verified_unchanged_units(tmp_path):
    adapter, path, row, _ = backend(tmp_path)
    class Journal:
        revision = 1
        def read_stamp(self):
            return self.revision
        def assert_unchanged(self, stamp):
            from backend.interface import EmbeddingContractError
            if self.revision != stamp:
                raise EmbeddingContractError('INDEX_CHANGED_DURING_SEARCH')
    reader = ReviewSourceReader(adapter, ['D']); reader.journal = Journal()
    scroll, calls = adapter.aclient.scroll, []
    async def counted(**kwargs):
        calls.append(kwargs)
        return await scroll(**kwargs)
    adapter.aclient.scroll = counted
    selector = {'dataset_id': 'D', 'file_name': 'source.txt'}
    first = asyncio.run(reader.read(selector))
    assert asyncio.run(reader.read(selector)) == first and len(calls) == 1
    reader.journal.revision += 1
    asyncio.run(reader.read(selector))
    assert len(calls) == 2
    path.write_text('Новый исходник', encoding='utf-8')
    with pytest.raises(ValueError, match='обновите индекс'):
        asyncio.run(reader.read(selector))
    assert len(calls) == 2  # Cached index text cannot bypass the full original SHA check.


def test_native_qdrant_review_filters_and_resumes_without_a_model(tmp_path, monkeypatch):
    from qdrant_client import AsyncQdrantClient, models
    from uuid import UUID
    monkeypatch.setenv('LES_PRODUCT_EDITION', 'light')
    adapter, path, row, _ = backend(tmp_path)
    async def scenario():
        client = AsyncQdrantClient(location=':memory:')
        try:
            await client.create_collection('review_trial', vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
            payload = {'dataset_id': 'D', 'file_name': 'source.txt', 'parent_id': 's1', 'page': 1, 'chunk_ord': 0}
            await client.upsert('review_trial', points=[
                models.PointStruct(id=str(UUID(int=1)), vector=[1., 0.], payload={**payload, 'text': 'Требуется 12 м.'}),
                models.PointStruct(id=str(UUID(int=2)), vector=[1., 0.], payload={**payload, 'text': 'Навигация', 'node_role': 'navigation'}),
                models.PointStruct(id=str(UUID(int=3)), vector=[1., 0.], payload={**payload, 'dataset_id': 'other', 'text': 'Чужие данные'}),
            ])
            adapter.aclient, adapter.collection_name = client, 'review_trial'
            reader = ReviewSourceReader(adapter, ['D'])
            store = ReviewTaskStore(tmp_path/'native-tasks.db')
            tool = DocumentReviewTool(store, reader, 'owner', 'Проверить')
            async def execute(arguments):
                payload = await tool.execute(arguments)
                tool.remember(arguments, payload)
                return payload

            started = await execute({'operation': 'start',
                'selectors': [{'dataset_id': 'D', 'file_name': 'source.txt'}]})
            assert started['task_id']
            data = await execute({'operation': 'read'})
            assert data['fragment_count'] == 1 and data['text'] == 'Требуется 12 м.'
            messages, _ = govern_inference_messages(preset=_FACTORY_9B, profile_prefix='Read', request_payload='Проверить',
                                                   **document_tool_context(tool))
            tool.acknowledge_model_context(messages)
            saved = await execute({'operation': 'record', 'request_id': 'save',
                'expected_revision': 0, 'unit_id': data['unit_id'], 'source_fingerprint': data['source_fingerprint'],
                'status': 'reviewed', 'note': 'Модель прочитала требование',
                'evidence': [{'fragment_id': data['fragment_id'], 'quote': '12 м'}]})
            assert saved['coverage_complete']
            task_id = saved['task_id']
            restarted = DocumentReviewTool(ReviewTaskStore(store.path), reader, 'owner', 'Продолжить')
            assert (await restarted.execute({'operation': 'status', 'task_id': task_id}))['reviewed'] == 1
            path.write_bytes(b'X' + path.read_bytes()[1:])
            status = await restarted.execute({'operation': 'status', 'task_id': task_id})
            assert status['needs_recheck'] == 1 and not status['coverage_complete']
        finally:
            await client.close()
    asyncio.run(scenario())


def test_next_action_keeps_the_unit_explicitly_chosen_by_the_model(tmp_path):
    selectors=[{'dataset_id':'D','file_name':f'file{i}.pdf'} for i in range(2)]
    async def read(selector, **kwargs):
        return {**snapshot('А😀Б'*500), 'selector':selector}
    reader=SimpleNamespace(allowed=frozenset(['D']),files=frozenset(),journal=None,read=read)
    tool=DocumentReviewTool(ReviewTaskStore(tmp_path/'tasks.db'),reader,'owner','Проверить',max_chars=203)
    start=asyncio.run(tool.execute({'operation':'start','selectors':selectors}))
    second=start['units'][1]['unit_id']
    result=asyncio.run(tool.execute({'operation':'read','unit_id':second}))
    hint=tool.context()['next_action']
    assert hint['unit_id']==second and hint['operation']=='read'
    assert hint['offset']==result['next_cursor']['offset']==203


def test_literal_quote_can_cross_delivered_cursor_boundary_but_not_a_gap(tmp_path):
    store,task,(unit,)=setup_store(tmp_path,1)
    snap=snapshot('А😀Б12 мХ😀Я')
    deliver(store,task,unit,snap,stop=5)
    deliver(store,task,unit,snap,start=5)
    assert record(store,task,unit,0,snap=snap)==1
    missing=store.create('owner','Другой разбор',[store.task('owner',task)['units'][0]['selector']])
    deliver(store,missing,unit,snap,stop=5)
    deliver(store,missing,unit,snap,start=6)
    with pytest.raises(ValueError,match='прочитанной'):
        record(store,missing,unit,0,snap=snap)


def test_excerpt_offset_needs_delivery_even_when_same_text_was_read_elsewhere(tmp_path):
    selector={'dataset_id':'D','file_name':'repeated.txt'}
    async def read(selector, **kwargs):
        return {**snapshot('А'*400), 'selector':selector}
    reader=SimpleNamespace(allowed=frozenset(['D']),files=frozenset(),journal=None,read=read)
    tool=DocumentReviewTool(ReviewTaskStore(tmp_path/'tasks.db'),reader,'owner','Проверить',max_chars=200)
    started=asyncio.run(tool.execute({'operation':'start','selectors':[selector]}))
    unit=started['units'][0]['unit_id']
    asyncio.run(tool.execute({'operation':'read'}))
    from proxy.services.tabular_document_service import encode
    def delivered():
        tool.acknowledge_model_context([{'role':'tool','content':encode({'document_state':tool.context()})}])
    delivered()
    page=asyncio.run(tool.execute({'operation':'read','offset':200}))
    args={'operation':'record','unit_id':unit,'expected_revision':0,'request_id':'one',
          'source_fingerprint':page['source_fingerprint'],'status':'needs_review','note':'Нужно уточнение',
          'evidence':[{'read_id':page['read_id'],'span_id':'q1'}]}
    with pytest.raises(ValueError,match='прочитанной'):
        asyncio.run(tool.execute(args))
    delivered()
    assert asyncio.run(tool.execute(args))['saved_revision']==1
    # An identical receipt still replays after the current cursor moves.
    asyncio.run(tool.execute({'operation':'read','unit_id':unit,'offset':0}))
    assert asyncio.run(tool.execute(args))['saved_revision']==1
    assert len(tool.store.history('owner',tool.task_id)['events'])==1
