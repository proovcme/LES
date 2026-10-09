"""Fuse Qdrant dense and exact local BM25 with the existing RRF rank constant."""
import asyncio
from qdrant_client import models
from backend import bm25_store
from backend.interface import EmbeddingContractError


def fuse(dense, lexical, limit):
    scores, points = {}, {}
    for channel in (dense, lexical):
        for position, point in enumerate(channel):
            points.setdefault(point.id, point)
            scores[point.id] = scores.get(point.id, 0.) + 1. / (2. + position)
    return [points[key].model_copy(update={"score": score}) for key, score in
            sorted(scores.items(), key=lambda row: row[1], reverse=True)[:limit]]


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
    return dense.model_copy(update={"points": fuse(dense.points, lexical_points, limit)})
