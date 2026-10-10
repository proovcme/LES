"""Dense/sparse retrieval, reranking and hierarchy."""
from __future__ import annotations
from backend import qdrant_support as support
from backend.embedding_client import EmbedClient
from backend.index_replacement import ReplacementJournal


def _query_embedder(adapter):
    journal = ReplacementJournal.for_adapter(adapter)
    stamp = journal.read_stamp()
    client = adapter.embed
    frozen = client.for_index(support._embedding_cache_descriptor()) if isinstance(client, EmbedClient) else client
    return frozen, journal, stamp


class QdrantRetrieval:
    async def retrieve(
        self,
        query:       str,
        dataset_ids: support.Optional[support.List[str]] = None,
        top_k:       int = 5,
        doc_filter:  support.Optional[support.List[str]] = None,
    ) -> support.List[support.Chunk]:
        await self._ensure_collection()
        self._assert_dense_index_contract()

        # Async эмбеддинг запроса
        embedder, journal, stamp = _query_embedder(self)
        vecs = await embedder.encode_async([query], query=True)
        query_vec = vecs[0]

        # ADR-12 стадия-2: doc_filter сужает поиск до выбранных документов-узлов
        # (file_name), а не по всему датасету. Пусто → прежнее поведение (плоско по датасету).
        must = []
        if dataset_ids:
            must.append(support.models.FieldCondition(key="dataset_id", match=support.models.MatchAny(any=dataset_ids)))
        if doc_filter:
            must.append(support.models.FieldCondition(key="file_name", match=support.models.MatchAny(any=doc_filter)))
        query_filter = support.models.Filter(must=must) if must else None

        results = await self.aclient.query_points(
            collection_name=self.collection_name,
            query=query_vec,
            using=support._dense_vector_name() if support._qdrant_schema_mode() == "named" else None,
            query_filter=query_filter,
            limit=top_k,
            with_payload=True,
        )

        def _is_binary_garbage(text: str) -> bool:
            """Detect base64-encoded or binary garbage chunks."""
            if not text or len(text) < 40:
                return False
            lines = text.split("\n")
            long_dense_lines = sum(
                1 for line in lines
                if len(line) > 60 and " " not in line and "/" in line + "=" in line
            )
            if long_dense_lines >= 2:
                return True
            # Check if text has no Cyrillic at all and looks like base64
            sample = text[:200].replace("\n", "")
            if len(sample) > 80:
                cyrillic = sum(1 for c in sample if "\u0400" <= c <= "\u04ff")
                spaces = sample.count(" ")
                if cyrillic == 0 and spaces < 3:
                    return True
            return False

        journal.assert_unchanged(stamp)
        return [
            support.Chunk(
                content=p.payload.get("text", ""),
                doc_id=p.payload.get("doc_id", ""),
                doc_name=p.payload.get("file_name", "unknown"),
                score=p.score,
                meta={**p.payload, "qdrant_point_id": str(p.id)},
            )
            for p in results.points
            if not _is_binary_garbage(p.payload.get("text", ""))
        ]

    async def retrieve_native_hybrid(
        self,
        query: str,
        dataset_ids: support.Optional[support.List[str]] = None,
        top_k: int = 8,
        doc_filter: support.Optional[support.List[str]] = None,
        node_roles: support.Optional[support.List[str]] = None,
        ancestor_ids: support.Optional[support.List[str]] = None,
        _query_state=None,
    ) -> support.List[support.Chunk]:
        """Qdrant dense + exact BM25 postings, with legacy native RRF for TF rollback.

        Requires `RAG_QDRANT_SCHEMA=named` and points containing both dense and
        sparse named vectors. Caller should keep a fallback to the legacy hybrid.
        """
        if support._qdrant_schema_mode() != "named":
            raise RuntimeError("qdrant native hybrid requires RAG_QDRANT_SCHEMA=named")
        from backend.inference.bm25_sparse import encode_bm25

        await self._ensure_collection()
        self._assert_dense_index_contract()
        if _query_state is None:
            if hasattr(self, "_prepare_sparse_index"):
                await self._prepare_sparse_index()
            embedder, journal, stamp = _query_embedder(self)
            dense_vec = (await embedder.encode_async([query], query=True))[0]
        else:
            dense_vec, journal, stamp = _query_state
        journal.assert_unchanged(stamp)
        dynamic = False
        if hasattr(self, "_prepare_sparse_index"):
            from backend.sparse_index import read_contract, STORAGE
            from backend.inference.lexical_tokens import tokenize_current
            dynamic = (read_contract(journal) or {}).get("storage") == STORAGE
            from backend.sparse_index import encode_query
            sparse = {term: 1. for term in tokenize_current(query)} if dynamic else encode_query(journal, query)
        else:
            sparse = encode_bm25(query)

        must = []
        if dataset_ids:
            must.append(support.models.FieldCondition(key="dataset_id", match=support.models.MatchAny(any=dataset_ids)))
        if doc_filter:
            must.append(support.models.FieldCondition(key="file_name", match=support.models.MatchAny(any=doc_filter)))
        if node_roles:
            must.append(support.models.FieldCondition(key="node_role", match=support.models.MatchAny(any=node_roles)))
        if ancestor_ids:
            must.append(
                support.models.FieldCondition(
                    key="ancestor_ids",
                    match=support.models.MatchAny(any=ancestor_ids),
                )
            )
        query_filter = support.models.Filter(must=must) if must else None
        prefetch_limit = max(top_k * 2, 24)
        if not sparse:
            results = await self.aclient.query_points(
                collection_name=self.collection_name, query=dense_vec,
                using=support._dense_vector_name(), query_filter=query_filter,
                limit=top_k, with_payload=True)
        elif dynamic:
            from backend.bm25_hybrid import query as dynamic_query
            results = await dynamic_query(self, journal, query, dense_vec, query_filter,
                dense_name=support._dense_vector_name(), prefetch_limit=prefetch_limit, limit=top_k,
                dataset_ids=dataset_ids, doc_filter=doc_filter, node_roles=node_roles, ancestor_ids=ancestor_ids)
        else:
            results = await self.aclient.query_points(
                collection_name=self.collection_name,
                prefetch=[
                    support.models.Prefetch(
                        query=dense_vec,
                        using=support._dense_vector_name(),
                        filter=query_filter,
                        limit=prefetch_limit,
                    ),
                    support.models.Prefetch(
                        query=support.models.SparseVector(indices=list(sparse.keys()), values=list(sparse.values())),
                        using=support._sparse_vector_name(),
                        filter=query_filter,
                        limit=prefetch_limit,
                    ),
                ],
                query=support.models.FusionQuery(fusion=support.models.Fusion.RRF),
                limit=top_k,
                with_payload=True,
            )
        journal.assert_unchanged(stamp)
        return [
            support.Chunk(
                content=p.payload.get("text", ""),
                doc_id=p.payload.get("doc_id", ""),
                doc_name=p.payload.get("file_name", "unknown"),
                score=p.score,
                meta={**p.payload, "qdrant_point_id": str(p.id),
                      "_retrieval_channels": ["dense", "bm25_postings" if dynamic else "qdrant_sparse"] if sparse else ["dense"]},
            )
            for p in results.points
            if len(p.payload.get("text", "")) >= 1
        ]

    async def retrieve_native_hierarchical(
        self,
        query: str,
        dataset_ids: support.Optional[support.List[str]] = None,
        top_k: int = 8,
        doc_filter: support.Optional[support.List[str]] = None,
    ) -> support.List[support.Chunk]:
        """Soft hierarchy: global evidence plus nav-routed descendant evidence.

        Navigation hits only produce filters.  They never enter the returned
        evidence pool, and the unfiltered global leg is always retained.
        """
        from backend.rag_hierarchy import reciprocal_rank_fuse

        await self._ensure_collection()
        self._assert_dense_index_contract()
        if hasattr(self, "_prepare_sparse_index"):
            await self._prepare_sparse_index()
        embedder, journal, stamp = _query_embedder(self)
        dense_vec = (await embedder.encode_async([query], query=True))[0]
        query_state = dense_vec, journal, stamp

        global_evidence = await self.retrieve_native_hybrid(
            query,
            dataset_ids=dataset_ids,
            top_k=top_k,
            doc_filter=doc_filter,
            node_roles=["evidence"],
            _query_state=query_state,
        )
        navigation = await self.retrieve_native_hybrid(
            query,
            dataset_ids=dataset_ids,
            top_k=min(max(4, top_k // 2), 16),
            doc_filter=doc_filter,
            node_roles=["navigation"],
            _query_state=query_state,
        )
        route_ids = [
            str((item.meta or {}).get("node_id") or "")
            for item in navigation
            if str((item.meta or {}).get("node_id") or "")
        ]
        if not route_ids:
            return global_evidence
        descendant_evidence = await self.retrieve_native_hybrid(
            query,
            dataset_ids=dataset_ids,
            top_k=top_k,
            doc_filter=doc_filter,
            node_roles=["evidence"],
            ancestor_ids=route_ids,
            _query_state=query_state,
        )
        return reciprocal_rank_fuse(
            [global_evidence, descendant_evidence],
            limit=top_k,
        )

    async def rerank_colbert(
        self,
        query: str,
        chunks: list[support.Chunk],
        *,
        top_k: int,
        max_query_tokens: int = 48,
        vector_name: str = "colbert",
    ) -> list[support.Chunk]:
        """Qdrant MaxSim rerank over an RRF shortlist of exact evidence ids."""
        import asyncio
        from backend.colbert_late_interaction import BgeM3ColbertEncoder

        point_ids = [
            str((chunk.meta or {}).get("qdrant_point_id") or "")
            for chunk in chunks
        ]
        point_ids = [point_id for point_id in point_ids if point_id]
        if len(point_ids) != len(chunks):
            raise RuntimeError("COLBERT_POINT_ID_MISSING")
        encoder = getattr(self, "_colbert_encoder", None)
        if encoder is None:
            encoder = BgeM3ColbertEncoder("BAAI/bge-m3")
            self._colbert_encoder = encoder
        query_vectors = (
            await asyncio.to_thread(
                encoder.encode, [query], max_length=max_query_tokens
            )
        )[0]
        results = await self.aclient.query_points(
            collection_name=self.collection_name,
            query=query_vectors,
            using=vector_name,
            query_filter=support.models.Filter(
                must=[support.models.HasIdCondition(has_id=point_ids)]
            ),
            limit=min(max(1, int(top_k)), len(chunks)),
            with_payload=False,
        )
        by_id = {point_id: chunk for point_id, chunk in zip(point_ids, chunks, strict=True)}
        ranked: list[support.Chunk] = []
        seen: set[str] = set()
        for rank, point in enumerate(results.points, start=1):
            point_id = str(point.id)
            chunk = by_id.get(point_id)
            if chunk is None:
                continue
            seen.add(point_id)
            chunk.meta["colbert_rank"] = rank
            chunk.meta["colbert_score"] = float(point.score or 0.0)
            ranked.append(chunk)
        ranked.extend(chunk for point_id, chunk in by_id.items() if point_id not in seen)
        return ranked

    async def retrieve_raptor_evidence(
        self,
        query: str,
        *,
        target_collection: str,
        source_collection: str,
        dataset_ids: support.Optional[support.List[str]] = None,
        doc_filter: support.Optional[support.List[str]] = None,
        route_k: int = 8,
        top_k: int = 32,
    ) -> support.List[support.Chunk]:
        """Route through summary nodes, then return only exact evidence leaves."""
        from backend.inference.bm25_sparse import encode_bm25

        aliases = await self.aclient.get_aliases()
        active_physical = next(
            (
                str(item.collection_name)
                for item in aliases.aliases
                if str(item.alias_name) == self.collection_name
            ),
            self.collection_name,
        )
        if active_physical != str(source_collection or ""):
            raise RuntimeError("RAPTOR_SOURCE_GENERATION_STALE")
        dense = (await self.embed.encode_async([query], query=True))[0]
        sparse = encode_bm25(query)
        if not sparse:
            raise RuntimeError("RAPTOR_SPARSE_QUERY_EMPTY")
        must = [
            support.models.FieldCondition(
                key="node_role", match=support.models.MatchValue(value="navigation")
            )
        ]
        if dataset_ids:
            must.append(
                support.models.FieldCondition(
                    key="dataset_id", match=support.models.MatchAny(any=dataset_ids)
                )
            )
        if doc_filter:
            must.append(
                support.models.FieldCondition(
                    key="file_name", match=support.models.MatchAny(any=doc_filter)
                )
            )
        query_filter = support.models.Filter(must=must)
        routes = await self.aclient.query_points(
            collection_name=target_collection,
            prefetch=[
                support.models.Prefetch(
                    query=dense,
                    using=support._dense_vector_name(),
                    filter=query_filter,
                    limit=max(16, int(route_k) * 2),
                ),
                support.models.Prefetch(
                    query=support.models.SparseVector(
                        indices=list(sparse), values=list(sparse.values())
                    ),
                    using=support._sparse_vector_name(),
                    filter=query_filter,
                    limit=max(16, int(route_k) * 2),
                ),
            ],
            query=support.models.FusionQuery(fusion=support.models.Fusion.RRF),
            limit=max(1, int(route_k)),
            with_payload=True,
        )
        leaf_routes: dict[str, tuple[int, str]] = {}
        for route_rank, point in enumerate(routes.points, start=1):
            payload = dict(point.payload or {})
            route_id = str(payload.get("node_id") or point.id)
            for leaf_id in payload.get("descendant_leaf_ids") or []:
                leaf_routes.setdefault(str(leaf_id), (route_rank, route_id))
                if len(leaf_routes) >= max(1, int(top_k)):
                    break
            if len(leaf_routes) >= max(1, int(top_k)):
                break
        if not leaf_routes:
            return []
        points = await self.aclient.retrieve(
            collection_name=self.collection_name,
            ids=list(leaf_routes),
            with_payload=True,
            with_vectors=False,
        )
        by_id = {str(point.id): point for point in points}
        chunks: list[support.Chunk] = []
        for leaf_id, (route_rank, route_id) in leaf_routes.items():
            point = by_id.get(leaf_id)
            if point is None:
                continue
            payload = dict(point.payload or {})
            if str(payload.get("node_role") or "") != "evidence":
                continue
            text = str(payload.get("text") or "")
            if not text:
                continue
            chunks.append(
                support.Chunk(
                    content=text,
                    doc_id=str(payload.get("doc_id") or ""),
                    doc_name=str(payload.get("file_name") or "unknown"),
                    score=1.0 / (60.0 + route_rank),
                    meta={
                        **payload,
                        "qdrant_point_id": leaf_id,
                        "raptor_route_id": route_id,
                        "raptor_route_rank": route_rank,
                    },
                )
            )
        return chunks

    async def retrieve_table_rows(
        self,
        dataset_ids: support.Optional[support.List[str]] = None,
        limit: int = 64,
    ) -> support.List[support.Chunk]:
        await self._ensure_collection()

        must = [
            support.models.FieldCondition(
                key="type",
                match=support.models.MatchValue(value="table_row"),
            )
        ]
        if dataset_ids:
            must.append(
                support.models.FieldCondition(
                    key="dataset_id",
                    match=support.models.MatchAny(any=dataset_ids),
                )
            )

        points, _next_page = await self.aclient.scroll(
            collection_name=self.collection_name,
            scroll_filter=support.models.Filter(must=must),
            limit=limit,
            with_payload=True,
            with_vectors=False,
        )

        return [
            support.Chunk(
                content=point.payload.get("text", ""),
                doc_id=point.payload.get("doc_id", ""),
                doc_name=point.payload.get("file_name", "unknown"),
                score=1.0,
                meta=point.payload,
            )
            for point in points
        ]

