from dataclasses import replace
import logging
from types import SimpleNamespace

import pytest

from backend.bm25_hybrid import fuse
from backend.interface import Chunk
from proxy.services import retrieval_service as service
from proxy.services.retrieval_trial_policy import CURRENT, MINIMAL
from tools.evaluate_rag_pipeline import evidence_metrics, summarize


@pytest.mark.asyncio
async def test_minimal_preserves_rrf_order_while_current_exact_promotions_are_measurable(monkeypatch):
    ordinary = Chunk('related passage', 'a', 'other.md', .9, {'qdrant_point_id': 'a'})
    exact = Chunk('another passage', 'b', 'specific-manual.pdf', .1, {'qdrant_point_id': 'b'})
    calls = []
    class Backend:
        collection_name = 'trial'
        async def retrieve_native_hybrid(self, question, **kwargs):
            calls.append(('flat', kwargs))
            return [ordinary, exact]
        async def retrieve_native_hierarchical(self, question, **kwargs):
            calls.append(('hierarchy', kwargs))
            return [ordinary, exact]
    # A source-name promotion is a real later stage; the flat trial bypasses it.
    monkeypatch.setattr(service, '_promote_exact_source_matches', lambda chunks, question: (list(reversed(chunks)), ['specific-manual.pdf']))
    monkeypatch.setattr(service, '_hybrid_merge', lambda *args, **kwargs: pytest.fail('Disabled lexical safety was called'))
    kwargs = dict(question='Что указано в specific-manual.pdf?', dataset_ids=['ds'], rag_backend=Backend(),
        reranker_enabled=False, reranker_available=False, reranker_cls=None, mlx_url='', logger=logging.getLogger(__name__), return_trace=True)
    simple = await service.retrieve_chat_chunks(**kwargs, trial_policy=MINIMAL)
    assert simple.chunks[0].doc_id == 'a'
    assert calls[0][1]['node_roles'] == ['evidence']
    assert 'hierarchical' not in simple.trace.mode
    promoted = await service.retrieve_chat_chunks(**kwargs, trial_policy=replace(CURRENT, lexical_safety=False))
    assert promoted.chunks[0].doc_id == 'b'
    assert calls[1][0] == 'hierarchy'


@pytest.mark.asyncio
async def test_empty_scope_is_blocked_even_in_trial():
    result = await service.retrieve_chat_chunks(question='q', dataset_ids=[], rag_backend=None,
        reranker_enabled=False, reranker_available=False, reranker_cls=None, mlx_url='',
        logger=logging.getLogger(__name__), return_trace=True, trial_policy=MINIMAL)
    assert result.trace.status == 'blocked'


def test_rrf_constant_changes_head_vs_consensus_without_changing_default():
    point = lambda id: SimpleNamespace(id=id, model_copy=lambda update: SimpleNamespace(id=id, **update))
    a, c = point('head'), point('consensus')
    dense = [a, point('x'), point('y'), c]
    lexical = [point('z'), point('v'), point('w'), c]
    low = {p.id: p.score for p in fuse(dense, lexical, 20)}
    high = {p.id: p.score for p in fuse(dense, lexical, 20, rank_constant=60)}
    assert low['head'] > low['consensus']
    assert high['head'] < high['consensus']


def test_metrics_distinguish_any_hit_all_support_and_unanswerable():
    case = {'gold_groups': [{'support': [{'point_id': 'rule'}]}, {'support': [{'point_id': 'exception'}]}]}
    chunks = [Chunk('rule', '', '', 0, {'qdrant_point_id': 'rule'})]
    metrics = evidence_metrics(chunks, case)
    assert metrics['recall5'] == .5 and metrics['mrr'] == 1 and not metrics['complete_support']
    assert evidence_metrics(chunks, {})['recall5'] is None
    # Failed answerable cases remain in the denominator.
    result = summarize([{'answerable': True, 'error': 'read failure'}])
    assert result['errors'] == 1 and result['model_packet']['complete_support'] == 0
    assert result['semantic_accuracy'] is None
