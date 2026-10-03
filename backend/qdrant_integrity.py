"""Index reconciliation, integrity and file-level mutations."""
from __future__ import annotations
from backend import qdrant_support as support


class QdrantIntegrity:
    def _file_filter(self, dataset_id: str, file_key: str) -> support.models.Filter:
        return support.models.Filter(must=[
            support.models.FieldCondition(
                key="file_name",
                match=support.models.MatchValue(value=file_key),
            ),
            support.models.FieldCondition(
                key="dataset_id",
                match=support.models.MatchValue(value=dataset_id),
            ),
        ])

    def _sync_delete_file_points(
        self,
        sync_qdrant: support.qdrant_client.QdrantClient,
        dataset_id: str,
        file_key: str,
    ) -> None:
        sync_qdrant.delete(
            collection_name=self.collection_name,
            points_selector=support.models.FilterSelector(
                filter=self._file_filter(dataset_id, file_key)
            ),
            wait=True,
        )

    def _sync_delete_file_lexical(self, dataset_id: str, file_key: str) -> None:
        try:
            from proxy.services.lexical_index_service import LexicalIndex

            deleted = LexicalIndex().delete_file(
                self.collection_name,
                dataset_id=dataset_id,
                doc_name=file_key,
            )
            if deleted:
                support.logger.debug("[LEXICAL] удалены старые FTS-чанки %s/%s: %s", dataset_id, file_key, deleted)
        except Exception as error:  # noqa: BLE001
            support.logger.warning("[LEXICAL] cleanup skipped %s/%s: %s", dataset_id, file_key, error)

    @staticmethod
    def _lexical_rows_from_points(points: list[support.Any]) -> list[dict[str, support.Any]]:
        rows: list[dict[str, support.Any]] = []
        for point in points:
            payload = getattr(point, "payload", None) or {}
            text = str(payload.get("text") or "")
            point_id = str(getattr(point, "id", "") or "")
            if not text.strip() or not point_id:
                continue
            rows.append({
                "point_id": point_id,
                "dataset_id": payload.get("dataset_id"),
                "doc_id": payload.get("doc_id"),
                "doc_name": payload.get("file_name") or payload.get("doc_name"),
                "text": text,
                "content_hash": payload.get("content_hash"),
                "chunk_ord": payload.get("chunk_ord"),
                "section_heading": payload.get("section_heading"),
                "parent_id": payload.get("parent_id"),
                "parent_ord": payload.get("parent_ord"),
                "child_ord": payload.get("child_ord"),
                "parent_heading": payload.get("parent_heading"),
                "context_before": payload.get("context_before"),
                "context_after": payload.get("context_after"),
                "context_kind": payload.get("context_kind"),
            })
        return rows

    def _sync_upsert_file_lexical(self, points: list[support.Any]) -> None:
        rows = self._lexical_rows_from_points(points)
        if not rows:
            return
        try:
            from proxy.services.lexical_index_service import LexicalIndex

            indexed = LexicalIndex().upsert_chunks(self.collection_name, rows)
            support.logger.debug("[LEXICAL] upsert FTS chunks collection=%s count=%s", self.collection_name, indexed)
        except Exception as error:  # noqa: BLE001
            support.logger.warning("[LEXICAL] upsert skipped collection=%s: %s", self.collection_name, error)

    def _sync_count_file_points(
        self,
        sync_qdrant: support.qdrant_client.QdrantClient,
        dataset_id: str,
        file_key: str,
    ) -> int:
        result = sync_qdrant.count(
            collection_name=self.collection_name,
            count_filter=self._file_filter(dataset_id, file_key),
            exact=True,
        )
        return int(result.count)

    def reconcile_dataset(self, dataset_id: str) -> dict:
        """РЕКОНСАЙЛ MetaDB↔Qdrant: для каждого INDEXED-документа сверяет chunk_count (SQLite) с числом
        точек в Qdrant. Несовпавшие → PENDING (переиндексируются и встанут на место). Лечит рассинхрон
        от сбоя cleanup/краша/ручных операций. Дорогая (count на файл) — это разовая операция-ремонт."""
        sync_qdrant = support.qdrant_client.QdrantClient(
            url=self.qdrant_url, timeout=60.0, check_compatibility=False,
            **support.qdrant_client_options(self.qdrant_url),
        )
        files = self.db.indexed_files_with_counts(dataset_id)
        checked = 0
        mismatched: list[dict] = []
        requeued_files: set[str] = set()
        for file_name, sqlite_cc in files:
            checked += 1
            try:
                qpoints = self._sync_count_file_points(sync_qdrant, dataset_id, file_name)
            except Exception as error:  # noqa: BLE001
                support.logger.warning("[RECONCILE] count failed %s: %s", file_name, error)
                continue
            dense_mismatch = qpoints != sqlite_cc
            if dense_mismatch:
                mismatched.append({"file": file_name, "sqlite": sqlite_cc, "qdrant": qpoints})
            if dense_mismatch:
                self.db.update_document_status(dataset_id, file_name, "PENDING", 0)
                requeued_files.add(file_name)
        if mismatched:
            self.db.update_dataset_chunk_count(dataset_id)
        support.logger.info("[RECONCILE] dataset=%s checked=%s mismatched=%s (→PENDING)",
                    dataset_id, checked, len(mismatched))
        return {"dataset_id": dataset_id, "checked": checked,
                "mismatched": len(mismatched), "requeued": len(requeued_files),
                "details": mismatched[:50]}

    def _sync_dataset_point_projection(
        self,
        sync_qdrant: support.qdrant_client.QdrantClient,
        dataset_id: str,
        *,
        collection_name: str | None = None,
    ) -> dict[str, dict[str, support.Any]]:
        projection: dict[str, dict[str, support.Any]] = {}
        offset = None
        dataset_filter = support.models.Filter(
            must=[
                support.models.FieldCondition(
                    key="dataset_id",
                    match=support.models.MatchValue(value=dataset_id),
                )
            ]
        )
        while True:
            points, offset = sync_qdrant.scroll(
                collection_name=collection_name or self.collection_name,
                scroll_filter=dataset_filter,
                limit=256,
                offset=offset,
                with_payload=True,
                with_vectors=False,
            )
            for point in points:
                payload = getattr(point, "payload", None) or {}
                file_name = str(payload.get("file_name") or payload.get("doc_name") or "")
                row = projection.setdefault(
                    file_name,
                    {"ids": set(), "pages": set(), "navigation_points": 0},
                )
                row["ids"].add(str(getattr(point, "id", "") or ""))
                if str(payload.get("node_role") or "") == "navigation":
                    row["navigation_points"] += 1
                if payload.get("type") == "pdf_page_text" and payload.get("page") is not None:
                    try:
                        row["pages"].add(int(payload["page"]))
                    except (TypeError, ValueError):
                        pass
            if offset is None:
                break
        return projection

    def _discover_generation_source_collection(
        self,
        sync_qdrant: support.qdrant_client.QdrantClient,
    ) -> str:
        contract = support.index_contract_status().get("actual") or {}
        explicit = str(contract.get("generation_source_collection") or "").strip()
        if explicit and sync_qdrant.collection_exists(explicit):
            return explicit
        expected = int(contract.get("generation_source_points") or 0)
        if expected <= 0:
            return ""
        aliases = {
            item.alias_name: item.collection_name
            for item in sync_qdrant.get_aliases().aliases
        }
        active = aliases.get(self.collection_name, self.collection_name)
        matches: list[str] = []
        for item in sync_qdrant.get_collections().collections:
            name = str(item.name)
            if name == active:
                continue
            try:
                if int(sync_qdrant.count(name, exact=True).count or 0) == expected:
                    matches.append(name)
            except Exception:
                continue
        return matches[0] if len(matches) == 1 else ""

    def reconcile_legacy_navigation_counts(
        self,
        *,
        apply: bool = False,
        source_collection: str | None = None,
    ) -> dict[str, support.Any]:
        """Repair stale MetaDB counters created by hierarchy-expanding sibling builds.

        This never deletes or reindexes content. A row is eligible only when every
        point above the stored count is explicitly a navigation point and dense,
        sparse and lexical projections all contain the same exact point ids/count.
        """
        sync_qdrant = getattr(self, "_catalog_sync_qdrant", None)
        owns_client = sync_qdrant is None
        if sync_qdrant is None:
            sync_qdrant = support.qdrant_client.QdrantClient(
                url=self.qdrant_url,
                **support.qdrant_client_options(self.qdrant_url),
                timeout=60.0,
                check_compatibility=False,
            )
        source_collection = str(source_collection or "").strip() or (
            self._discover_generation_source_collection(sync_qdrant)
        )
        candidates: list[tuple[str, str, int, int]] = []
        rejected: list[dict[str, support.Any]] = []
        checked_files = 0
        try:
            for dataset in self.db.list_datasets():
                if int(dataset.indexed_files or 0) <= 0:
                    continue
                dataset_id = str(dataset.id)
                projection = self._sync_dataset_point_projection(sync_qdrant, dataset_id)
                source_projection: dict[str, dict[str, support.Any]] = {}
                if source_collection:
                    source_projection = self._sync_dataset_point_projection(
                        sync_qdrant,
                        dataset_id,
                        collection_name=source_collection,
                    )
                lexical = self.db.lexical_integrity_projection(dataset_id)
                lexical_files: dict[str, set[str]] = lexical.get("files") or {}
                for file_name, expected in self.db.indexed_files_with_counts(dataset_id):
                    checked_files += 1
                    qrow = projection.get(file_name) or {
                        "ids": set(),
                        "navigation_points": 0,
                    }
                    qids = set(qrow.get("ids") or set())
                    actual = len(qids)
                    if actual <= expected:
                        continue
                    navigation = int(qrow.get("navigation_points") or 0)
                    dense = self._sync_count_file_vector_points(
                        sync_qdrant,
                        dataset_id,
                        file_name,
                        support._dense_vector_name(),
                    )
                    sparse = self._sync_count_file_vector_points(
                        sync_qdrant,
                        dataset_id,
                        file_name,
                        support._sparse_vector_name(),
                    )
                    lexical_matches = set(lexical_files.get(file_name) or set()) == qids
                    source_row = source_projection.get(file_name) or {}
                    source_ids = set(source_row.get("ids") or set())
                    source_actual = len(source_ids) if source_collection else None
                    source_navigation = (
                        int(source_row.get("navigation_points") or 0)
                        if source_collection
                        else None
                    )
                    if support._legacy_navigation_count_candidate(
                        expected=int(expected),
                        actual=actual,
                        navigation=navigation,
                        dense=dense,
                        sparse=sparse,
                        lexical_matches=lexical_matches,
                        source_actual=source_actual,
                        source_navigation=source_navigation,
                    ):
                        candidates.append((dataset_id, file_name, int(expected), actual))
                    else:
                        rejected.append(
                            {
                                "dataset_id": dataset_id,
                                "file": file_name,
                                "expected": int(expected),
                                "actual": actual,
                                "navigation": navigation,
                                "dense": dense,
                                "sparse": sparse,
                                "lexical_matches": lexical_matches,
                                "source_actual": source_actual,
                                "source_navigation": source_navigation,
                            }
                        )
        finally:
            if owns_client:
                sync_qdrant.close()

        repairs = [
            (dataset_id, file_name, actual)
            for dataset_id, file_name, _expected, actual in candidates
        ]
        updated = (
            self.db.apply_document_chunk_count_repairs(repairs)
            if apply and not rejected
            else 0
        )
        return {
            "schema": "les.rag.navigation-count-reconcile.v1",
            "status": (
                "blocked"
                if rejected
                else "repaired"
                if updated
                else "ready"
                if not candidates
                else "repairable"
            ),
            "checked_files": checked_files,
            "candidate_files": len(candidates),
            "candidate_points": sum(actual for _, _, _, actual in candidates),
            "candidate_delta": sum(actual - expected for _, _, expected, actual in candidates),
            "rejected_files": len(rejected),
            "updated_files": updated,
            "source_collection": source_collection,
            "details": rejected[:20],
        }

    @staticmethod
    def _expected_pdf_text_pages(path: support.Path) -> set[int]:
        if path.suffix.lower() not in support.PDF_PAGE_NODE_SUFFIXES:
            return set()
        try:
            import pdfplumber

            with pdfplumber.open(path) as document:
                return {
                    page_no
                    for page_no, page in enumerate(document.pages, start=1)
                    if len((page.extract_text() or "").strip()) >= support.MIN_CHUNK
                }
        except Exception as error:  # noqa: BLE001
            support.logger.warning("[INTEGRITY] PDF page inventory failed %s: %s", path, error)
            return set()

    def _sync_count_file_vector_points(
        self,
        sync_qdrant: support.qdrant_client.QdrantClient,
        dataset_id: str,
        file_key: str,
        vector_name: str,
    ) -> int:
        file_filter = self._file_filter(dataset_id, file_key)
        file_filter.must.append(support.models.HasVectorCondition(has_vector=vector_name))
        result = sync_qdrant.count(
            collection_name=self.collection_name,
            count_filter=file_filter,
            exact=True,
        )
        return int(result.count)

    def audit_dataset_integrity(self, dataset_id: str, *, repair: bool = False) -> dict[str, support.Any]:
        """Verify one dataset across source, MetaDB, Qdrant dense/sparse, lexical and FTS.

        Repair is conservative: only damaged documents are requeued; missing sources are marked
        MISSING, FTS is rebuilt from its content table, and points with no registered document are
        removed.  The parse job itself is started by the API layer so the operator sees one job.
        """
        sync_qdrant = support.qdrant_client.QdrantClient(
            url=self.qdrant_url,
            **support.qdrant_client_options(self.qdrant_url),
            timeout=60.0,
            check_compatibility=False,
        )
        rows = self.db.dataset_integrity_rows(dataset_id)
        registry = {str(row["file_name"]): row for row in rows}
        try:
            qdrant_files = self._sync_dataset_point_projection(sync_qdrant, dataset_id)
        except Exception as error:  # noqa: BLE001
            return {
                "schema": "les.dataset_integrity.v1",
                "dataset_id": dataset_id,
                "state": "blocked",
                "label": "Векторная база недоступна",
                "checked_files": len(rows),
                "indexed_files": 0,
                "clean_files": 0,
                "damaged_files": 0,
                "missing_files": 0,
                "contract_ok": False,
                "fts_ok": False,
                "orphan_qdrant_files": 0,
                "orphan_lexical_files": 0,
                "repaired": 0,
                "requeued": 0,
                "issues": [{"file": "", "status": "BLOCKED", "problems": [str(error)[:300]]}],
            }
        lexical = self.db.lexical_integrity_projection(dataset_id)
        lexical_files: dict[str, set[str]] = lexical.get("files") or {}
        contract = support.index_contract_status()
        contract_ok = bool(contract.get("compatible"))

        damaged: set[str] = set()
        missing: set[str] = set()
        issues: list[dict[str, support.Any]] = []
        clean_files = 0
        indexed_files = 0
        pending_files = 0
        file_checks: list[dict[str, support.Any]] = []

        for file_name, row in registry.items():
            status = str(row.get("status") or "")
            source = support.Path(str(row.get("source_path") or "")) if row.get("source_path") else (
                self.content_dir / dataset_id / file_name
            )
            file_issues: list[str] = []
            expected = int(row.get("chunk_count") or 0)
            qdrant_count = dense_count = sparse_count = lexical_count = 0
            expected_page_count = indexed_page_count = 0
            if status == "ERROR":
                file_issues.append("Предыдущая индексация завершилась ошибкой")
                damaged.add(file_name)
            elif status == "PENDING":
                pending_files += 1
                file_issues.append("Ожидает индексации")
            if status != "SKIPPED":
                if not source.is_file():
                    file_issues.append("Исходный файл не найден")
                    missing.add(file_name)
                else:
                    stat = source.stat()
                    expected_size = int(row.get("file_size") or 0)
                    expected_mtime = float(row.get("file_mtime") or 0)
                    if expected_size and stat.st_size != expected_size:
                        file_issues.append("Исходный файл изменился")
                        damaged.add(file_name)
                    elif expected_mtime and abs(stat.st_mtime - expected_mtime) > 1.0:
                        stored_hash = str(row.get("file_hash") or "")
                        if stored_hash and support._sha256_file(source) != stored_hash:
                            file_issues.append("Содержимое исходного файла изменилось")
                            damaged.add(file_name)

            if status == "INDEXED":
                indexed_files += 1
                qrow = qdrant_files.get(file_name) or {"ids": set(), "pages": set()}
                qids = set(qrow.get("ids") or set())
                lexical_ids = set(lexical_files.get(file_name) or set())
                dense = self._sync_count_file_vector_points(
                    sync_qdrant, dataset_id, file_name, support._dense_vector_name()
                )
                sparse = self._sync_count_file_vector_points(
                    sync_qdrant, dataset_id, file_name, support._sparse_vector_name()
                )
                qdrant_count = len(qids)
                dense_count = dense
                sparse_count = sparse
                lexical_count = len(lexical_ids)
                if len(qids) != expected:
                    file_issues.append(f"Векторный индекс: {len(qids)} из {expected}")
                if dense != expected:
                    file_issues.append(f"Смысловой поиск: {dense} из {expected}")
                if sparse != expected:
                    file_issues.append(f"Точный поиск: {sparse} из {expected}")
                if lexical_ids != qids:
                    file_issues.append(f"Текстовый поиск: {len(lexical_ids)} из {len(qids)}")
                if source.is_file() and source.suffix.lower() in support.PDF_PAGE_NODE_SUFFIXES:
                    expected_pages = self._expected_pdf_text_pages(source)
                    indexed_pages = set(qrow.get("pages") or set())
                    expected_page_count = len(expected_pages)
                    indexed_page_count = len(indexed_pages)
                    if expected_pages and indexed_pages != expected_pages:
                        file_issues.append(
                            f"Страницы PDF: {len(indexed_pages)} из {len(expected_pages)}"
                        )
                if file_issues and file_name not in missing:
                    damaged.add(file_name)

            if file_issues:
                issues.append({"file": file_name, "status": status, "problems": file_issues})
            elif status == "INDEXED":
                clean_files += 1
            file_checks.append(
                {
                    "file": file_name,
                    "status": status,
                    "expected_chunks": expected,
                    "qdrant_chunks": qdrant_count,
                    "dense_chunks": dense_count,
                    "sparse_chunks": sparse_count,
                    "lexical_chunks": lexical_count,
                    "expected_text_pages": expected_page_count,
                    "indexed_text_pages": indexed_page_count,
                    "problems": list(file_issues),
                }
            )

        orphan_qdrant = sorted(set(qdrant_files) - set(registry))
        orphan_lexical = sorted(set(lexical_files) - set(registry))
        fts_ok = bool(lexical.get("fts_available")) and (
            set(lexical.get("lexical_ids") or set()) == set(lexical.get("fts_ids") or set())
        )
        if not contract_ok:
            issues.insert(0, {"file": "", "status": "BLOCKED", "problems": ["Контракт индекса не совпадает"]})
        if orphan_qdrant:
            issues.append({"file": "", "status": "ORPHAN", "problems": [f"Лишние векторные документы: {len(orphan_qdrant)}"]})
        if orphan_lexical:
            issues.append({"file": "", "status": "ORPHAN", "problems": [f"Лишние текстовые документы: {len(orphan_lexical)}"]})
        if not fts_ok:
            issues.append({"file": "", "status": "FTS", "problems": ["Текстовый индекс требует пересборки"]})

        repaired = 0
        if repair and contract_ok:
            for file_name in sorted(missing):
                self._sync_delete_file_points(sync_qdrant, dataset_id, file_name)
                self._sync_delete_file_lexical(dataset_id, file_name)
            self.db.set_documents_missing(dataset_id, missing)
            repaired += self.db.set_documents_pending(dataset_id, damaged - missing)
            for file_name in orphan_qdrant:
                point_ids = list((qdrant_files.get(file_name) or {}).get("ids") or [])
                if point_ids:
                    sync_qdrant.delete(
                        collection_name=self.collection_name,
                        points_selector=support.models.PointIdsList(points=point_ids),
                        wait=True,
                    )
            for file_name in orphan_lexical:
                self._sync_delete_file_lexical(dataset_id, file_name)
            if not fts_ok and lexical.get("fts_available"):
                self.db.rebuild_lexical_fts()
            self.db.update_dataset_chunk_count(dataset_id)
            repaired += len(missing) + len(orphan_qdrant) + len(orphan_lexical) + (0 if fts_ok else 1)

        state = (
            "blocked"
            if not contract_ok or missing
            else "repairable"
            if damaged or orphan_qdrant or orphan_lexical or not fts_ok
            else "building"
            if pending_files
            else "healthy"
        )
        return {
            "schema": "les.dataset_integrity.v1",
            "dataset_id": dataset_id,
            "state": state,
            "label": {
                "healthy": "Датасет цел",
                "repairable": "Найдены исправимые повреждения",
                "building": "Индексация не завершена",
                "blocked": "Нужно внимание оператора",
            }[state],
            "checked_files": len(rows),
            "indexed_files": indexed_files,
            "pending_files": pending_files,
            "clean_files": clean_files,
            "damaged_files": len(damaged),
            "missing_files": len(missing),
            "contract_ok": contract_ok,
            "fts_ok": fts_ok,
            "orphan_qdrant_files": len(orphan_qdrant),
            "orphan_lexical_files": len(orphan_lexical),
            "repaired": repaired,
            "requeued": len(damaged - missing) if repair and contract_ok else 0,
            "issues": issues[:100],
            "file_checks": file_checks,
        }

    def _sync_existing_file_vectors_by_hash(
        self,
        sync_qdrant: support.qdrant_client.QdrantClient,
        dataset_id: str,
        file_key: str,
        embedding_fingerprint: str,
    ) -> dict[str, list[float]]:
        vectors: dict[str, list[float]] = {}
        offset = None
        try:
            while True:
                points, offset = sync_qdrant.scroll(
                    collection_name=self.collection_name,
                    scroll_filter=self._file_filter(dataset_id, file_key),
                    limit=256,
                    offset=offset,
                    with_payload=True,
                    with_vectors=True,
                )
                for point in points:
                    payload = getattr(point, "payload", None) or {}
                    if str(payload.get("embedding_fingerprint") or "") != embedding_fingerprint:
                        continue
                    text = str(payload.get("text") or "")
                    content_hash = str(payload.get("content_hash") or support._content_hash(text))
                    vector = self._extract_point_vector(point)
                    if content_hash and vector is not None:
                        vectors.setdefault(content_hash, vector)
                if offset is None:
                    break
        except Exception as error:
            support.logger.warning("[PARSE] chunk hash cache unavailable for %s: %s", file_key, error)
        return vectors

    @staticmethod
    def _extract_point_vector(point: support.Any) -> list[float] | None:
        vector = getattr(point, "vector", None)
        if isinstance(vector, dict):
            vector = (
                vector.get("")
                or vector.get("default")
                or vector.get(support._dense_vector_name())
                or next((v for v in vector.values() if isinstance(v, list)), None)
            )
        if isinstance(vector, list) and vector and all(isinstance(item, (int, float)) for item in vector):
            return [float(item) for item in vector]
        return None

