"""Small frozen-index RAG candidate, restricted to isolated trial collections."""
from qdrant_client import models

from backend.inference.bm25_sparse import encode_bm25
from backend.inference.bm25_weighted import BM25Profile
from proxy.services.section_evidence_candidate import Fragment, pack_sections


def sparse_vector(values):
    return models.SparseVector(indices=list(values), values=list(values.values()))


class RetrievalCandidate:
    def __init__(self, client, collection: str, profile: BM25Profile):
        if not collection.startswith("les_trial_"):
            raise ValueError("Candidate requires an isolated les_trial_ collection")
        self.client, self.collection, self.profile = client, collection, profile

    def search(self, query, dense, dataset_ids, *, weighted=True, limit=10, candidate_k=100,
               rerank=None):
        if not dataset_ids or not 1 <= limit <= candidate_k <= 256:
            raise ValueError("Explicit datasets and bounded search limits are required")
        sparse = self.profile.query(query) if weighted else encode_bm25(query)
        if not sparse:
            return []
        scope = models.Filter(must=[
            models.FieldCondition(key="dataset_id", match=models.MatchAny(any=dataset_ids)),
        ], must_not=[models.FieldCondition(key="node_role", match=models.MatchValue(value="navigation")),
                    models.FieldCondition(key="evidence_admissible", match=models.MatchValue(value=False))])
        points = self.client.query_points(self.collection, prefetch=[
            models.Prefetch(query=dense, using="dense", filter=scope, limit=candidate_k),
            models.Prefetch(query=sparse_vector(sparse), using="bm25_sparse" if weighted else "sparse_tf",
                            filter=scope, limit=candidate_k),
        ], query=models.FusionQuery(fusion=models.Fusion.RRF),
            limit=candidate_k if rerank else limit, with_payload=True).points
        hits = [Fragment.from_point(point) for point in points]
        if rerank is not None:
            # Explicit optional stage; a failure is never labelled successful reranking.
            ids = list(rerank(query, hits))
            by_id = {hit.point_id: hit for hit in hits}
            if len(ids) != len(hits) or len(set(ids)) != len(ids) or set(ids) != set(by_id):
                raise ValueError("Reranker must return a permutation of the supplied evidence IDs")
            hits = [by_id[value] for value in ids]
        return hits[:limit]

    def read_section(self, hit: Fragment):
        if not hit.parent_id:
            points = self.client.retrieve(self.collection, ids=[hit.point_id], with_payload=True)
            return [Fragment.from_point(point) for point in points]
        scope = models.Filter(must=[models.FieldCondition(key=key, match=models.MatchValue(value=value))
            for key, value in (("dataset_id", hit.dataset_id), ("file_name", hit.file_name),
                               ("parent_id", hit.parent_id))],
            must_not=[models.FieldCondition(key="node_role", match=models.MatchValue(value="navigation")),
                      models.FieldCondition(key="evidence_admissible", match=models.MatchValue(value=False))])
        points, offset = [], None
        while True:
            page, offset = self.client.scroll(self.collection, scroll_filter=scope, offset=offset,
                                             limit=128, with_payload=True, with_vectors=False)
            points.extend(page)
            if len(points) > 2048:
                raise ValueError("Indexed section exceeds the trial safety limit")
            if offset is None:
                break
        return [Fragment.from_point(point) for point in points]

    def context(self, hits, *, max_tokens, count_tokens):
        return pack_sections(hits, self.read_section, max_tokens=max_tokens, count_tokens=count_tokens)
