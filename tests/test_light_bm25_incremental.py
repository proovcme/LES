"""Incremental correctness and publication boundaries, without any model."""
import json
import random
from types import SimpleNamespace

import pytest
from qdrant_client import models
from backend import bm25_store, sparse_index
from backend.bm25_hybrid import fuse
from backend.index_replacement import ReplacementJournal
from backend.interface import EmbeddingContractError
from test_light_bm25_index import index, native_index, put, oracle, scores


def test_random_changes_match_full_scalar_oracle_without_full_scroll(index, monkeypatch):
    client, journal = index
    rng = random.Random(757)
    words = ['кабель', 'автомат', 'PE', '63', 'сечение', 'ЩР/2']
    texts = [' '.join(rng.choices(words, k=rng.randrange(1, 40))) for _ in range(40)]
    put(client, texts)
    before = client.retrieve('docs', ids=list(range(40)), with_vectors=True)
    sparse_index.ensure_current(client, journal)
    after = client.retrieve('docs', ids=list(range(40)), with_vectors=True)
    assert before == after  # Migration does not even rewrite legacy sparse vectors.
    monkeypatch.setattr(client, 'scroll', lambda *a, **k: pytest.fail('full corpus scan'))
    monkeypatch.setattr(client, 'update_vectors', lambda *a, **k: pytest.fail('vector rewrite'))
    for _ in range(20):
        pid = rng.randrange(len(texts))
        texts[pid] = ' '.join(rng.choices(words, k=rng.randrange(0, 50)))
        with sparse_index.external_mutation(journal, ids=[pid]):
            put(client, [texts[pid]], pid)
        # New journal object exercises durable state instead of in-process cache.
        journal = ReplacementJournal(journal.directory.parents[1], 'docs')
        sparse_index.ensure_current(client, journal)
        assert scores(client, journal, 'кабель PE 63') == pytest.approx(oracle(texts, 'кабель PE 63'), rel=1e-10)
        assert journal.sparse_changes() == []


def test_file_replacement_and_dataset_delete_scope(index, monkeypatch):
    client, journal = index
    put(client, ['кабель', 'кабель кабель', 'автомат'])
    client.set_payload('docs', payload={'dataset_id':'other'}, points=[2])
    sparse_index.ensure_current(client, journal)
    original = client.scroll
    seen = []
    def scoped(*args, **kwargs):
        assert kwargs.get('scroll_filter') is not None
        seen.append(kwargs['scroll_filter'])
        return original(*args, **kwargs)
    monkeypatch.setattr(client, 'scroll', scoped)
    with journal.lease():
        journal.begin('а', 'Путь с пробелами.pdf', 'Путь с пробелами.pdf', ['new-id'])
        put(client, ['сечение'], 0)
        journal.commit()
        journal.finish()
    sparse_index.ensure_current(client, journal)
    assert set(scores(client, journal, 'кабель')) == {1}
    with sparse_index.external_mutation(journal, dataset='а'):
        client.delete('docs', models.PointIdsList(points=[0, 1]))
    result = sparse_index.ensure_current(client, journal)
    assert result['points'] == 1 and scores(client, journal, 'кабель') == {}
    assert set(scores(client, journal, 'автомат')) == {2}
    assert len(seen) == 2


def test_filters_precede_limit_and_do_not_change_global_idf(index):
    client, journal = index
    put(client, ['кабель'] * 8)
    client.set_payload('docs', payload={'dataset_id':'selected','file_name':'target',
        'node_role':'evidence','ancestor_ids':['parent']}, points=[7])
    sparse_index.ensure_current(client, journal)
    all_scores = scores(client, journal, 'кабель')
    filtered = bm25_store.search(journal, 'кабель', dataset_ids=['selected'], doc_filter=['target'],
                                node_roles=['evidence'], ancestor_ids=['parent'], limit=1)
    assert filtered == [(7, all_scores[7])]
    assert not bm25_store.search(journal, 'кабель', ancestor_ids=['other'])
    assert not bm25_store.search(journal, 'кабель', node_roles=['navigation'])


@pytest.mark.parametrize('boundary', ['postings', 'contract', 'publication'])
def test_crash_in_incremental_refresh_is_blocked_and_recovers(index, monkeypatch, boundary):
    client, journal = index
    texts = ['кабель', 'сечение']
    put(client, texts)
    sparse_index.ensure_current(client, journal)
    with sparse_index.external_mutation(journal, ids=[0]):
        texts[0] = 'сечение сечение'
        put(client, texts[:1])
    target, name = {'postings':(bm25_store,'put'), 'contract':(sparse_index,'_save_contract'),
                    'publication':(journal,'publish_sparse')}[boundary]
    original = getattr(target, name)
    def crash(*args, **kwargs):
        if boundary == 'postings':
            original(*args, **kwargs)
        raise OSError('simulated interruption')
    monkeypatch.setattr(target, name, crash)
    with pytest.raises(OSError):
        sparse_index.ensure_current(client, journal)
    with pytest.raises(EmbeddingContractError):
        bm25_store.search(journal, 'сечение')
    monkeypatch.setattr(target, name, original)
    with journal.lease():
        journal.recover(client, SimpleNamespace())
    assert scores(client, journal, 'сечение') == pytest.approx(oracle(texts, 'сечение'))


def test_query_rejects_generation_change(index, monkeypatch):
    client, journal = index
    put(client, ['кабель'])
    sparse_index.ensure_current(client, journal)
    original = journal.assert_unchanged
    def changed(stamp):
        with sparse_index.external_mutation(journal, ids=[0]):
            put(client, ['автомат'])
        original(stamp)
    monkeypatch.setattr(journal, 'assert_unchanged', changed)
    with pytest.raises(EmbeddingContractError, match='INDEX_CHANGED_DURING_SEARCH'):
        bm25_store.search(journal, 'кабель')


def test_unknown_changes_rebuild_and_empty_corpus_repopulates(index):
    client, journal = index
    put(client, ['кабель'])
    sparse_index.ensure_current(client, journal)
    with sparse_index.external_mutation(journal):
        client.delete('docs', models.PointIdsList(points=[0]))
    assert sparse_index.ensure_current(client, journal)['points'] == 0
    assert scores(client, journal, 'кабель') == {}
    with sparse_index.external_mutation(journal, ids=[1]):
        put(client, ['PE'], 1)
    assert sparse_index.ensure_current(client, journal)['points'] == 1
    assert list(scores(client, journal, 'PE')) == [1]


def test_rrf_matches_qdrant_rank_formula_and_keeps_input_scores():
    from qdrant_client.hybrid.fusion import reciprocal_rank_fusion
    dense = [models.ScoredPoint(id=i, score=10-i, version=0) for i in [1,2,3]]
    lexical = [models.ScoredPoint(id=i, score=5, version=0) for i in [3,1,4]]
    actual = fuse(dense, lexical, 4)
    assert dense[0].score == 9
    expected = reciprocal_rank_fusion([dense, lexical], limit=4)
    assert [(p.id,p.score) for p in actual] == [(p.id,p.score) for p in expected]


@pytest.mark.asyncio
async def test_native_hierarchical_route_uses_postings(native_index, monkeypatch):
    from qdrant_client import AsyncQdrantClient
    from backend.qdrant_retrieval import QdrantRetrieval
    client, journal = native_index
    monkeypatch.setenv('RAG_QDRANT_SCHEMA', 'named')
    put(client, ['Подключение PE', 'Другой документ', 'Навигация PE'])
    client.set_payload('docs', payload={'node_role':'evidence','ancestor_ids':['section']}, points=[0,1])
    client.set_payload('docs', payload={'node_role':'navigation','node_id':'section'}, points=[2])
    class Embed:
        async def encode_async(self, *a, **kw): return [[1.,0.]]
    class Adapter(QdrantRetrieval):
        async def _ensure_collection(self): pass
        def _assert_dense_index_contract(self): pass
        async def _prepare_sparse_index(self): return sparse_index.ensure_current(client, journal)
    adapter = Adapter()
    adapter.collection_name, adapter.content_dir, adapter.embed = 'docs', journal.directory.parents[1], Embed()
    adapter.aclient = AsyncQdrantClient(**client.init_options)
    try:
        hits = await adapter.retrieve_native_hierarchical('PE', ['а'], top_k=2)
        assert hits[0].meta['qdrant_point_id'] == '0'
        assert all(hit.meta['node_role'] == 'evidence' for hit in hits)
        assert not await adapter.retrieve_native_hierarchical('PE', ['missing'], top_k=2)
    finally:
        await adapter.aclient.close()
