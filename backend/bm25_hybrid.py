"""Fuse Qdrant dense and exact local BM25 with the existing RRF rank constant."""
import asyncio
from qdrant_client import models
from backend import bm25_store, native_core
from backend.interface import EmbeddingContractError


def fuse(dense, lexical, limit, *, rank_constant=2.):
    if not 0 < rank_constant < float('inf'):
        raise ValueError('RRF rank constant must be finite and positive')
    points = {}
    for channel in (dense, lexical):
        for point in channel:
            points.setdefault(point.id, point)
    result = native_core.required('bm25', request=dict(action='fuse', rank_constant=rank_constant,
        limit=limit, channels=[[point.id for point in channel] for channel in (dense, lexical)]))
    return [points[key].model_copy(update={"score": score}) for key, score in result['hits']]


async def query(adapter, journal, text, dense_vec, query_filter, *, dense_name,
                prefetch_limit, limit, **filters):
    stamp = journal.read_stamp()
    dense, lexical = await asyncio.gather(
        adapter.aclient.query_points(collection_name=adapter.collection_name,
            query=dense_vec, using=dense_name, query_filter=query_filter,
            limit=prefetch_limit, with_payload=True),
        asyncio.to_thread(bm25_store.search, journal, text, limit=prefetch_limit, **filters))
    records = await adapter.aclient.retrieve(adapter.collection_name,
        ids=[point_id for point_id, _ in lexical], with_payload=True, with_vectors=False) if lexical else []
    by_id = {point.id: point for point in records}
    if len(by_id) != len(lexical):
        raise EmbeddingContractError("INDEX_CHANGED_DURING_SEARCH")
    lexical_points = [models.ScoredPoint(id=point_id, score=score, version=0,
        payload=by_id[point_id].payload) for point_id, score in lexical]
    journal.assert_unchanged(stamp)
    return dense.model_copy(update={"points": fuse(dense.points, lexical_points, limit,
        rank_constant=getattr(adapter, 'rrf_rank_constant', 2.))})
