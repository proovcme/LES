"""Collection lifecycle and health contract."""
from __future__ import annotations
from backend import qdrant_support as support


class QdrantCollection:
    async def _ensure_collection(self):
        if self._collection_ready:
            return
        async with self._collection_lock:
            if self._collection_ready:
                return
            created = False
            collection_info = None
            try:
                collection_info = await self.aclient.get_collection(self.collection_name)
            except Exception:
                support.logger.info(f"[INIT] Создаём коллекцию {self.collection_name}")
            if collection_info is not None and support._qdrant_schema_mode() == "named":
                compatible, points_count = support._named_collection_layout(
                    collection_info,
                    vector_size=self.vector_size,
                )
                if not compatible:
                    if points_count:
                        raise support.EmbeddingContractError(
                            f"collection {self.collection_name} has {points_count} points in an "
                            "incompatible vector schema; explicit migration is required"
                        )
                    support.logger.warning(
                        "[INIT] Пересоздаём пустую legacy-коллекцию %s как named dense+sparse",
                        self.collection_name,
                    )
                    await self.aclient.delete_collection(self.collection_name)
                    collection_info = None
                elif support.index_contract_status().get("status") == "missing":
                    # A packaged/clean Windows baseline may already contain the
                    # canonical named collection while its small sidecar file is
                    # absent. Adopt it only when the collection is empty or every
                    # existing point proves the current embedding identity.
                    matching_count = 0
                    if points_count:
                        expected_fingerprint = support.point_embedding_fingerprint()
                        matching = await self.aclient.count(
                            collection_name=self.collection_name,
                            count_filter=support.models.Filter(
                                must=[
                                    support.models.FieldCondition(
                                        key="embedding_fingerprint",
                                        match=support.models.MatchValue(value=expected_fingerprint),
                                    )
                                ]
                            ),
                            exact=True,
                        )
                        matching_count = int(matching.count or 0)
                    adopt = support._can_adopt_missing_contract(
                        points_count=points_count,
                        matching_fingerprint_count=matching_count,
                    )
                    if adopt:
                        support.write_index_contract(replace=False)
                        support.logger.info(
                            "[INIT] Adopted compatible named collection %s (%s points) into index contract",
                            self.collection_name,
                            points_count,
                        )
            if collection_info is None:
                if support._qdrant_schema_mode() == "named":
                    await self.aclient.create_collection(
                        collection_name=self.collection_name,
                        vectors_config={
                            support._dense_vector_name(): support.models.VectorParams(
                                size=self.vector_size,
                                distance=support.models.Distance.COSINE,
                            )
                        },
                        sparse_vectors_config={
                            support._sparse_vector_name(): support.models.SparseVectorParams(
                                modifier=support.models.Modifier.IDF,
                            )
                        },
                    )
                else:
                    await self.aclient.create_collection(
                        collection_name=self.collection_name,
                        vectors_config=support.models.VectorParams(
                            size=self.vector_size, distance=support.models.Distance.COSINE
                        ),
                    )
                created = True
            if created or (
                collection_info is not None
                and support._named_collection_layout(collection_info, vector_size=self.vector_size)[1] == 0
            ):
                try:
                    support.write_index_contract(replace=False)
                except FileExistsError:
                    # A pre-existing sidecar is validated below; never overwrite it
                    # implicitly during startup.
                    pass
            # Payload-индексы под фильтрованный поиск (retrieve фильтрует по dataset_id и
            # file_name). БЕЗ индекса query_points с фильтром проверяет фильтр по ВСЕМ точкам
            # (~1.6с на 179k) — с индексом ~30мс. create_payload_index идемпотентен (повторный
            # вызов — no-op/обновление). Best-effort: сбой не должен блокировать старт.
            if (
                support.payload_index_ensure_enabled()
                and (self._payload_index_task is None or self._payload_index_task.done())
            ):
                self._payload_index_task = support.asyncio.create_task(self._ensure_payload_indexes())
            self._collection_ready = True

    async def _ensure_payload_indexes(self) -> None:
        """Create filter indexes without holding FastAPI application startup."""
        for _field in (
            "dataset_id",
            "file_name",
            "embedding_fingerprint",
            "mail_account_id",
            "mail_thread_key",
            "mail_registry_message_id",
        ):
            try:
                await self.aclient.create_payload_index(
                    collection_name=self.collection_name,
                    field_name=_field,
                    field_schema=support.models.PayloadSchemaType.KEYWORD,
                    # Qdrant may serialize collection mutations immediately
                    # after a clean collection was created.  Waiting for
                    # every payload index made FastAPI startup look dead for
                    # minutes even though the operation was safely queued.
                    wait=False,
                )
            except Exception as _idx_err:  # noqa: BLE001
                support.logger.warning("[INIT] payload-индекс %s: %s", _field, _idx_err)

    @staticmethod
    def _assert_dense_index_contract() -> None:
        status = support.index_contract_status()
        if not status.get("compatible"):
            raise support.EmbeddingContractError(
                "index contract "
                f"{status.get('status')}: expected={status.get('expected_fingerprint', '')} "
                f"actual={status.get('actual_fingerprint', '') or 'none'}"
            )

    async def health(self) -> bool:
        try:
            timeout = max(
                1.0,
                min(float(support.os.getenv("LES_QDRANT_HEALTH_TIMEOUT_SEC", "4")), 12.0),
            )
            await support.asyncio.wait_for(self._ensure_collection(), timeout=timeout)
            return True
        except Exception:
            return False

    async def health_snapshot(self) -> support.Dict[str, support.Any]:
        ok = await self.health()
        snapshot = self.db.health_snapshot()
        try:
            from proxy.services.rag_catalog_guard_service import catalog_guard_state

            snapshot["catalog_guard"] = catalog_guard_state()
        except Exception as error:
            snapshot["catalog_guard"] = {
                "status": "unknown",
                "error_code": "RAG_CATALOG_GUARD_STATE_FAILED",
                "exception_type": type(error).__name__,
                "message": str(error) or type(error).__name__,
            }
        snapshot["qdrant"] = {"ok": ok, "collection": self.collection_name}
        contract = support.index_contract_status()
        snapshot["index_contract"] = contract
        snapshot["dense_available"] = bool(ok and contract.get("compatible"))
        if ok and not contract.get("compatible"):
            snapshot["status"] = "degraded"
        if ok:
            try:
                collection = await self.aclient.get_collection(self.collection_name)
                points = collection.points_count or 0
                snapshot["qdrant"]["points"] = points
                physical_collection = self.collection_name
                try:
                    aliases = await self.aclient.get_aliases()
                    physical_collection = next(
                        (
                            str(item.collection_name)
                            for item in aliases.aliases
                            if str(item.alias_name) == self.collection_name
                        ),
                        self.collection_name,
                    )
                except Exception:
                    pass
                snapshot["qdrant"]["physical_collection"] = physical_collection
                expected_point_fingerprint = str(
                    (contract.get("actual") or {}).get("point_embedding_fingerprint")
                    or support.point_embedding_fingerprint()
                )
                matching = await self.aclient.count(
                    collection_name=self.collection_name,
                    count_filter=support.models.Filter(
                        must=[
                            support.models.FieldCondition(
                                key="embedding_fingerprint",
                                match=support.models.MatchValue(value=expected_point_fingerprint),
                            )
                        ]
                    ),
                    exact=True,
                )
                matching_points = int(matching.count or 0)
                fingerprint_ready = support._point_fingerprint_coverage_ready(
                    points=points,
                    matching=matching_points,
                )
                snapshot["qdrant"]["compatible_fingerprint_points"] = matching_points
                snapshot["qdrant"]["point_fingerprint_match"] = fingerprint_ready
                evidence_count = await self.aclient.count(
                    collection_name=self.collection_name,
                    count_filter=support.models.Filter(
                        must=[
                            support.models.FieldCondition(
                                key="node_role",
                                match=support.models.MatchValue(value="evidence"),
                            )
                        ]
                    ),
                    exact=True,
                )
                navigation_count = await self.aclient.count(
                    collection_name=self.collection_name,
                    count_filter=support.models.Filter(
                        must=[
                            support.models.FieldCondition(
                                key="node_role",
                                match=support.models.MatchValue(value="navigation"),
                            )
                        ]
                    ),
                    exact=True,
                )
                snapshot["qdrant"]["evidence_points"] = int(evidence_count.count or 0)
                snapshot["qdrant"]["hierarchy_navigation_points"] = int(
                    navigation_count.count or 0
                )
                actual_contract = contract.get("actual") or {}
                colbert_required = bool(actual_contract.get("colbert_schema"))
                colbert_points = 0
                if colbert_required:
                    colbert_count = await self.aclient.count(
                        collection_name=self.collection_name,
                        count_filter=support.models.Filter(
                            must=[
                                support.models.HasVectorCondition(
                                    has_vector=str(
                                        actual_contract.get("colbert_vector_name") or "colbert"
                                    )
                                )
                            ]
                        ),
                        exact=True,
                    )
                    colbert_points = int(colbert_count.count or 0)
                snapshot["qdrant"]["colbert_required"] = colbert_required
                snapshot["qdrant"]["colbert_points"] = colbert_points
                snapshot["colbert_verified"] = bool(
                    colbert_required
                    and colbert_points == int(points)
                    and colbert_points > 0
                )
                snapshot["dense_available"] = bool(
                    ok and contract.get("compatible") and fingerprint_ready
                )
                if not fingerprint_ready:
                    snapshot["status"] = "degraded"
                support._apply_collection_count_health(snapshot, physical_points=int(points))
                guard_status = str((snapshot.get("catalog_guard") or {}).get("status") or "")
                if guard_status in {"degraded", "blocked"}:
                    snapshot["status"] = "degraded"
            except Exception as error:
                snapshot["qdrant"].update({"ok": False, "error": str(error)})
        try:
            from proxy.services.rag_pipeline_status_service import (
                build_retrieval_pipeline_status,
            )

            snapshot["retrieval_pipeline"] = build_retrieval_pipeline_status(snapshot)
        except Exception as error:
            snapshot["retrieval_pipeline"] = {
                "schema": "les.rag.retrieval-pipeline-status.v1",
                "status": "unknown",
                "error_code": "RAG_PIPELINE_STATUS_FAILED",
                "exception_type": type(error).__name__,
                "message": str(error) or type(error).__name__,
                "stages": {},
            }
        return snapshot

