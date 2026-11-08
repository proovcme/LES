"""Bounded document conversion and indexing pipeline."""
from __future__ import annotations
from backend import native_core, qdrant_support as support
from backend.qdrant_nodes import QdrantNodes
from backend.index_replacement import retire_previous, discard_staged, ReplacementJournal, recover_adapter
from backend.embedding_client import EmbedClient
from backend.extraction_errors import DocumentExtractionError
from backend.native_index_store import publication_directory, publish_file, upsert


def _compatibility_source_plan(db, dataset_id, data_dir, limit):
    get_pairs = getattr(db, "get_pending_files_with_paths", None)
    pairs = list(get_pairs(dataset_id, limit=limit)) if get_pairs is not None else [
        (name, "") for name in db.get_pending_files(dataset_id, limit=limit)
    ]
    external = {name: path for name, path in pairs if path}
    internal = {name for name, path in pairs if not path}
    all_files = [
        f for f in data_dir.rglob("*")
        if f.is_file() and "_parquet" not in f.relative_to(data_dir).parts
    ]
    relative = [(f, f.relative_to(data_dir).as_posix()) for f in all_files]
    legacy = internal - {name for _, name in relative}
    selected = [
        (f, name, name if name in internal else f.name)
        for f, name in relative if name in internal or f.name in legacy
    ]
    internal_count = len(selected)
    selected.extend((support.Path(path), name, name) for name, path in external.items())
    return len(pairs), selected, external, len(all_files), internal_count


class SourceRegistrationChanged(RuntimeError):
    """Discard work whose document was removed or registered differently."""


class QdrantIngestion:
    async def parse_dataset(self, dataset_id: str, limit: int | None = None, *,
                            sources: list[tuple[str, str]] | None = None,
                            intake_id: str | None = None) -> support.Dict[str, support.Any]:
        if limit is None and support.os.getenv(support.ALLOW_UNBOUNDED_PARSE, "").lower() not in ("1", "true", "yes"):
            return {
                "status": "rejected",
                "error": (
                    "unbounded parse is disabled; use parse_dataset(..., limit=N) "
                    f"or set {support.ALLOW_UNBOUNDED_PARSE}=1 explicitly"
                ),
            }
        await self._ensure_collection()
        self._assert_dense_index_contract()
        native_directory = publication_directory(self)
        if native_directory is not None:
            # Rust owns the durable job, every processing callback and cleanup.
            # Losing this HTTP caller must not cancel or duplicate that owner.
            return await support.asyncio.to_thread(self._sync_parse,dataset_id,limit,
                sources=sources,intake_id=intake_id,native_directory=native_directory)
        self.db.update_dataset_status(dataset_id, "PARSING")
        from backend import embedding_residency
        residency_token, owned_embeddings = embedding_residency.begin()
        parse_cancelled = False
        try:
            selection = {} if sources is None else {"sources": sources}
            if intake_id is not None:
                selection["intake_id"] = intake_id
            res = await support.asyncio.to_thread(self._sync_parse, dataset_id, limit, **selection)
        except BaseException as error:
            parse_cancelled = isinstance(error, support.asyncio.CancelledError)
            self.db.update_dataset_status(dataset_id, "ERROR")
            raise
        finally:
            try:
                # Cancelling to_thread does not stop its running function. Do not
                # unload its embedding model while that thread may still use it.
                embedding_cleanup = ({'released': 0, 'busy': True} if parse_cancelled
                                     else await embedding_residency.release(owned_embeddings))
                if 'res' in locals():res['embedding_cleanup'] = embedding_cleanup
            except Exception:
                support.logger.warning('Embedding cleanup failed; original outcome preserved', exc_info=True)
            finally:
                embedding_residency.end(residency_token)
            from backend.runtime_memory import collect_transient
            try:
                cleanup = await support.asyncio.to_thread(collect_transient, 'batch_end')
                if 'res' in locals():res['memory_cleanup'] = cleanup
            except Exception:
                support.logger.warning('Parse cleanup failed; original outcome preserved', exc_info=True)
        status = "COMPLETED" if res.get("status") == "completed" else "ERROR"
        if res.get("errors", 0) > 0:
            status = "ERROR"
        if res.get("remaining_pending", 0) > 0 and status == "COMPLETED":
            status = "IDLE" if limit is not None else "PARSING"
        self.db.update_dataset_status(dataset_id, status)
        return res

    @staticmethod
    def _source_fingerprint(path):
        try:
            value = native_core.call("fingerprint", path=str(path), include_mtime=True)
        except native_core.NativeCoreError as error:
            if str(error) == "CORE_FILE_CHANGED":
                raise RuntimeError("SOURCE_CHANGED_DURING_INDEXING") from error
            if str(error) in {"CORE_FILE_UNAVAILABLE", "CORE_NOT_REGULAR_FILE"}:
                raise DocumentExtractionError("INDEX_SOURCE_UNAVAILABLE") from error
            raise
        if value is not None and "mtime" in value:
            return dict(file_hash=value["sha256"], file_mtime=value["mtime"], file_size=value["size"])
        before = path.stat()
        digest = support._sha256_file(path)
        after = path.stat()
        if (before.st_size, before.st_mtime_ns, before.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino):
            raise RuntimeError("SOURCE_CHANGED_DURING_INDEXING")
        return dict(file_hash=digest, file_mtime=after.st_mtime, file_size=after.st_size)

    def _sync_parse(self, dataset_id: str, limit: int | None = None, *, sources=None, intake_id=None, native_directory=None) -> support.Dict[str, support.Any]:
        directory = native_directory if native_directory is not None else publication_directory(self)
        if directory is not None:
            if not native_core.supports('index_run'):
                raise native_core.NativeCoreError('CORE_CAPABILITY_MISMATCH')
            from backend.native_index_batch import run
            return run(self, dataset_id, limit, directory, sources=sources, intake_id=intake_id)
        recover_adapter(self)
        journal = ReplacementJournal.for_adapter(self)
        journal.assert_clean()
        return QdrantIngestion._sync_parse_locked(self, dataset_id, limit, journal, sources=sources, intake_id=intake_id)

    def _sync_parse_locked(self, dataset_id, limit, journal, *, sources=None, intake_id=None):
        """
        Синхронный парсинг в threadpool.
        Батч-эмбеддинги: 32 чанка за запрос вместо по одному.
        """
        import time as _t
        sync_qdrant = None
        t0 = _t.time()
        timings = {
            "delete_sec": 0.0,
            "route_sec": 0.0,
            "convert_sec": 0.0,
            "chunk_sec": 0.0,
            "embed_sec": 0.0,
            "upsert_sec": 0.0,
            "count_sec": 0.0,
            "cache_sec": 0.0,
            "db_sec": 0.0,
        }

        def _add_timing(key: str, started: float) -> None:
            timings[key] = timings.get(key, 0.0) + (_t.time() - started)

        data_dir = self.content_dir / dataset_id
        if not data_dir.exists():
            return {"status": "error", "msg": "dir missing"}

        md_parser = support.MarkdownNodeParser()
        # W2.1 (ADR-7): чанкинг в токенах эмбеддера (RAG_CHUNK_UNIT=chars вернёт символы).
        _chunking = support.chunking_config()
        splitter = support.StructureAwareSplitter(
            chunk_size=_chunking["chunk_size"],
            chunk_overlap=_chunking["chunk_overlap"],
            len_fn=_chunking["len_fn"],
        )
        support.logger.info(
            "[CHUNK] unit=%s size=%s overlap=%s",
            _chunking["unit"], _chunking["chunk_size"], _chunking["chunk_overlap"],
        )

        try:
            publisher = publication_directory(self)
            selection_errors = {}
            if sources is not None:
                pending_count = len(sources)
                external_sources = dict(sources)
                files_to_parse = [(support.Path(path), name, name) for name, path in sources]
                total_all = internal_count = 0
            elif publisher is not None:
                plan = native_core.required("index_sources", request={
                    "database": str(support.Path(self.db.db_path).resolve()),
                    "root": str(data_dir.resolve()), "dataset_id": dataset_id, "limit": limit,
                })
                files_to_parse = [(support.Path(row["path"]), row["file_key"], row["db_key"]) for row in plan["files"]]
                external_sources = {row["db_key"]: row["source_path"] for row in plan["files"] if row["source_path"]}
                selection_errors = {row["db_key"]: row["error"] for row in plan["files"] if row["error"]}
                pending_count = len(files_to_parse)
                total_all, internal_count = plan["total_files"], plan["matched_internal"]
            else:
                pending_count, files_to_parse, external_sources, total_all, internal_count = (
                    _compatibility_source_plan(self.db, dataset_id, data_dir, limit)
                )

            if not pending_count:
                return {
                    "status": "completed",
                    "chunks": 0,
                    "files_parsed": 0,
                    "files_skipped": total_all,
                    "remaining_pending": 0,
                    "errors": 0,
                    "elapsed_sec": 0,
                }

            total = len(files_to_parse)
            support.logger.info(
                f"[PARSE] {total}/{total_all} файлов к индексации (внешних in-place: {len(external_sources)})"
            )

            if total == 0:
                return {"status": "completed", "chunks": 0, "skipped": total_all}

            if publisher is None:
                sync_qdrant = support.qdrant_client.QdrantClient(
                    url=self.qdrant_url,
                    **support.qdrant_client_options(self.qdrant_url),
                    timeout=60.0,
                    check_compatibility=False,
                )

            total_chunks = 0
            errors = 0
            embedding_cache_hits = 0
            embedded_chunks = 0
            embedding_descriptor = support._embedding_cache_descriptor()
            embedding_fingerprint = support._embedding_cache_fingerprint(embedding_descriptor)

            # W1.4: конвейер — пока текущий файл эмбеддится/апсертится, следующий конвертируется
            # в фоновом потоке. OCR-файлы конвертируются в основном потоке (VLM не гоняем
            # параллельно с эмбеддером).
            convert_pool = (
                support.ThreadPoolExecutor(max_workers=1, thread_name_prefix="les-convert")
                if support.PARSE_PREFETCH and total > 1
                else None
            )
            _set_stage = getattr(self.db, "update_document_stage", None)

            def _stage(db_key: str, stage: str) -> None:
                if _set_stage is None:
                    return
                try:
                    _set_stage(dataset_id, db_key, stage)
                except Exception:
                    pass

            def _delete_file_lexical(file_key: str) -> None:
                delete_lexical = getattr(self, "_sync_delete_file_lexical", None)
                if delete_lexical is not None:
                    delete_lexical(dataset_id, file_key)

            def _upsert_file_lexical(points: list[support.Any]) -> None:
                upsert_lexical = getattr(self, "_sync_upsert_file_lexical", None)
                if upsert_lexical is not None:
                    upsert_lexical(points)

            def _convert_source(index: int, local_timings: dict, allow_ocr: bool):
                f, fk, dbk = files_to_parse[index]
                registration = self.db.document_registration(dataset_id, dbk)
                if registration is None or registration[1] != external_sources.get(dbk, ""):
                    raise SourceRegistrationChanged("Источник удалён или изменён; нужна повторная проверка каталога.")
                if code := selection_errors.get(dbk):
                    raise DocumentExtractionError(code)
                fingerprint = QdrantIngestion._source_fingerprint(f)
                route, nodes = QdrantIngestion._convert_file(
                    self, f, data_dir, fk, dataset_id, md_parser, splitter, local_timings, allow_ocr,
                )
                return route, nodes, fingerprint, registration

            def _submit_convert(index: int):
                local_timings: dict = {}
                future = convert_pool.submit(
                    _convert_source, index, local_timings, False,
                )
                return index, future, local_timings

            next_convert = _submit_convert(0) if convert_pool else None

            def process_one(index, attempts):
                nonlocal next_convert, convert_pool, total_chunks, errors, embedding_cache_hits, embedded_chunks
                if attempts == 0:
                    return 0  # The manifest entry changed after selection.
                i = index + 1
                file_path, file_key, db_file_key = files_to_parse[index]
                if i % 50 == 0 or i == total:
                    support.logger.info(f"[PARSE] {i}/{total} ({_t.time()-t0:.0f}с)")
                staged_ids: list[str] = []
                replacement_committed = False
                journal_started = False
                try:
                    _parse_embed = getattr(self, "embed_parse", None) or self.embed
                    if isinstance(_parse_embed, EmbedClient):
                        _parse_embed = _parse_embed.for_index(embedding_descriptor)
                    _stage(db_file_key, "CONVERT")
                    if next_convert is not None:
                        if next_convert[0] != index:
                            next_convert[1].cancel()
                            next_convert = _submit_convert(index)
                        _, future, local_timings = next_convert
                        try:
                            route, file_nodes, source_fingerprint, registration = future.result(timeout=support.PARSE_FILE_TIMEOUT)
                        except support.FuturesTimeoutError:
                            # Зависший конвертер бросаем вместе с пулом; индексация продолжается.
                            convert_pool.shutdown(wait=False, cancel_futures=True)
                            convert_pool = support.ThreadPoolExecutor(max_workers=1, thread_name_prefix="les-convert")
                            raise RuntimeError(
                                f"convert timeout: >{support.PARSE_FILE_TIMEOUT:.0f}s (поток конвертации брошен)"
                            )
                        finally:
                            for key, val in local_timings.items():
                                timings[key] = timings.get(key, 0.0) + val
                            next_convert = _submit_convert(i) if i < total else None
                        if file_nodes is None:
                            # OCR-конвейер: конвертируем синхронно в основном потоке.
                            route, file_nodes = QdrantIngestion._convert_file(
                                self, file_path, data_dir, file_key, dataset_id,
                                md_parser, splitter, timings, True,
                            )
                    else:
                        route, file_nodes, source_fingerprint, registration = _convert_source(i - 1, timings, True)

                    # One final invariant for every parser and node type. Parser-
                    # specific chunkers may improve boundaries, but none may bypass
                    # the actual embedding tokenizer budget or content sanitation.
                    phase_start = _t.time()
                    file_nodes = QdrantNodes._finalize_embedding_nodes(
                        file_nodes or [],
                        chunking=_chunking,
                    )
                    _add_timing("chunk_sec", phase_start)

                    # Hierarchy may append navigation nodes. It must run before
                    # sparse prevalidation so every final node has both channels.
                    support._apply_context_metadata_to_nodes(file_nodes, dataset_id, file_key)

                    # Native RRF is a corpus-wide invariant: a named-schema
                    # point is never allowed to enter the collection without
                    # its sparse companion.  Validate before deleting the old
                    # file points, so an invalid replacement cannot erase a
                    # previously usable document.
                    vector_names = None
                    if support._qdrant_schema_mode() == "named":
                        vector_names = {"dense": support._dense_vector_name(), "sparse": support._sparse_vector_name()}
                        if publisher is None:
                            file_nodes = support._prepare_named_sparse_nodes(file_nodes, file_key)

                    phase_start = _t.time()
                    existing_vectors = (
                        self._sync_existing_file_vectors_by_hash(
                            sync_qdrant,
                            dataset_id,
                            file_key,
                            embedding_fingerprint,
                        )
                        if publisher is None and support.CHUNK_HASH_CACHE and hasattr(self, "_sync_existing_file_vectors_by_hash")
                        else {}
                    )
                    _add_timing("cache_sec", phase_start)

                    if not file_nodes:
                        raise DocumentExtractionError('NO_SEARCHABLE_TEXT')

                    file_chunk_count = len(file_nodes)
                    verify_count = i % support.VERIFY_POINTS_EVERY == 0 or i == total
                    _stage(db_file_key, "EMBED")
                    if publisher is not None:
                        try:
                            result = publish_file(publisher, self.collection_name, dataset_id, file_key,
                                db_file_key, registration, source_fingerprint, route, file_nodes,
                                _parse_embed.encode_sync, embedding=embedding_descriptor,
                                embedding_fingerprint=embedding_fingerprint, vector_names=vector_names,
                                embed_batch=support.EMBED_BATCH, batch_size=support.UPSERT_BATCH,
                                cache_enabled=support.CHUNK_HASH_CACHE, verify_count=verify_count,
                                before_publish=lambda: _stage(db_file_key, "UPSERT"))
                        except support.EmbeddingContractError as error:
                            if str(error) == "INDEX_REGISTRATION_CHANGED":
                                raise SourceRegistrationChanged("Источник удалён или изменён; результат индексации отменён.") from error
                            if str(error) == "INDEX_SOURCE_ALIAS_CONFLICT":
                                raise DocumentExtractionError("INDEX_SOURCE_ALIAS_CONFLICT") from error
                            raise
                        except native_core.NativeCoreError as error:
                            if str(error) == "NO_SEARCHABLE_TEXT":
                                raise DocumentExtractionError("NO_SEARCHABLE_TEXT") from error
                            raise
                        for phase in ("cache_sec", "embed_sec", "upsert_sec"):
                            timings[phase] = timings.get(phase, 0.0) + result[phase]
                        embedding_cache_hits += result["cache_hits"]
                        embedded_chunks += result["embedded"]
                        total_chunks += result["count"]
                        if result["skipped_sparse"]:
                            support.logger.warning("Skipped %s non-searchable chunks in %s",
                                                   result["skipped_sparse"], file_key)
                        return 0

                    # Батч-эмбеддинги по EMBED_BATCH чанков. Upsert начинаем только
                    # после успешного embedding всех чанков файла, чтобы не оставлять
                    # частичный индекс при сбое середины документа.
                    points = []
                    for batch_start in range(0, len(file_nodes), support.EMBED_BATCH):
                        batch = file_nodes[batch_start:batch_start + support.EMBED_BATCH]
                        batch_vectors: list[list[float] | None] = [None] * len(batch)
                        miss_indexes: list[int] = []
                        miss_texts: list[str] = []
                        for local_idx, node in enumerate(batch):
                            payload = node.get("payload") or {}
                            content_hash = str(payload.get("content_hash") or support._content_hash(str(node["text"])))
                            cached_vector = existing_vectors.get(content_hash)
                            if cached_vector is not None:
                                batch_vectors[local_idx] = cached_vector
                                embedding_cache_hits += 1
                            else:
                                miss_indexes.append(local_idx)
                                miss_texts.append(str(node["text"]))

                        if miss_texts:
                            phase_start = _t.time()
                            # Парс-эмбеддер (EMBED_URL_PARSE); дефолт/тесты-моки → основной self.embed.
                            vectors = _parse_embed.encode_sync(miss_texts)
                            _add_timing("embed_sec", phase_start)
                            if len(vectors) != len(miss_texts):
                                raise RuntimeError(
                                    f"embedding count mismatch: got {len(vectors)}, expected {len(miss_texts)}"
                                )
                            embedded_chunks += len(vectors)
                            for local_idx, vec in zip(miss_indexes, vectors):
                                batch_vectors[local_idx] = vec

                        for node, vec in zip(batch, batch_vectors):
                            if vec is None:
                                raise RuntimeError("missing embedding vector after cache/embed merge")
                            payload = dict(node.get("payload") or {})
                            payload.update({
                                "text":       node["text"],
                                "dataset_id": dataset_id,
                                "doc_id":     node.get("doc_id") or str(support.uuid.uuid4()),
                                "file_name":  file_key,
                                "source_sha256": source_fingerprint["file_hash"],
                                "embedding_fingerprint": embedding_fingerprint,
                                "embedding_backend": embedding_descriptor.get("backend", ""),
                                "embedding_model_id": embedding_descriptor.get("model_id", ""),
                                "embedding_profile": embedding_descriptor.get("profile", ""),
                                "embedding_coreml_model": embedding_descriptor.get("coreml_model", ""),
                                "embedding_coreml_seq_len": embedding_descriptor.get("coreml_seq_len", ""),
                                "embedding_coreml_compute_units": embedding_descriptor.get("coreml_compute_units", ""),
                                "embedding_coreml_fallback": embedding_descriptor.get("coreml_fallback", ""),
                            })
                            point_vector: support.Any = vec
                            if vector_names is not None:
                                sparse_vec = node.pop("_rrf_sparse_vector", None)
                                if not sparse_vec:
                                    raise RuntimeError("missing prevalidated sparse vector")
                                point_vector = {
                                    vector_names["dense"]: vec,
                                    vector_names["sparse"]: support.models.SparseVector(
                                        indices=list(sparse_vec.keys()),
                                        values=list(sparse_vec.values()),
                                    ),
                                }
                            points.append(support.models.PointStruct(
                                id=str(support.uuid.uuid4()),
                                vector=point_vector,
                                payload=payload,
                            ))

                    # Verify the source generation before entering the legacy writer lease.
                    if QdrantIngestion._source_fingerprint(file_path) != source_fingerprint:
                        raise RuntimeError("SOURCE_CHANGED_DURING_INDEXING: original changed; previous index preserved")
                    with journal.lease():
                        journal.assert_clean()
                        if self.db.document_registration(dataset_id, db_file_key) != registration:
                            raise SourceRegistrationChanged("Источник удалён или изменён; результат индексации отменён.")
                        _stage(db_file_key, "UPSERT")
                        journal.begin(dataset_id, file_key, db_file_key, [str(point.id) for point in points])
                        journal_started = True
                        for point_start in range(0, len(points), support.UPSERT_BATCH):
                            phase_start = _t.time()
                            batch_points = points[point_start:point_start + support.UPSERT_BATCH]
                            # Include a possibly partially accepted batch in rollback.
                            staged_ids.extend(str(point.id) for point in batch_points)
                            upsert(sync_qdrant, self.collection_name, batch_points)
                            _add_timing("upsert_sec", phase_start)
                        journal.commit()
                        replacement_committed = True
                        retire_previous(sync_qdrant, self.collection_name, dataset_id, file_key, staged_ids)
                        replace_lexical = getattr(self, "_sync_replace_file_lexical", None)
                        if replace_lexical is not None:
                            replace_lexical(dataset_id, file_key, points)
                        else:
                            _delete_file_lexical(file_key)
                            _upsert_file_lexical(points)
                        self.db.clear_structured_rules(file_key)

                        # Exact count is sampled for the legacy writer.
                        if verify_count:
                            phase_start = _t.time()
                            indexed_points = self._sync_count_file_points(sync_qdrant, dataset_id, file_key)
                            _add_timing("count_sec", phase_start)
                            if indexed_points != file_chunk_count:
                                raise RuntimeError(
                                    f"qdrant point count mismatch: got {indexed_points}, expected {file_chunk_count}"
                                )
                        phase_start = _t.time()
                        self.db.update_document_status(
                            dataset_id, db_file_key, "INDEXED", file_chunk_count, route=route
                        )
                        set_fingerprint = getattr(self.db, "set_document_source_fingerprint", None)
                        if callable(set_fingerprint):
                            set_fingerprint(dataset_id, db_file_key, **source_fingerprint)
                        _add_timing("db_sec", phase_start)
                        self.db.update_dataset_chunk_count(dataset_id)
                        journal.finish()
                    total_chunks += file_chunk_count

                except SourceRegistrationChanged as file_err:
                    # Do not recreate a deleted row or mark a replacement row as
                    # failed. Nothing has been published for this stale ticket.
                    support.logger.warning("[PARSE] CANCELLED %s: %s", file_key, file_err)
                    errors += 1

                except support.UnsupportedIndexingSourceError as file_err:
                    support.logger.info("[PARSE] SKIPPED %s: %s", file_key, file_err)
                    phase_start = _t.time()
                    mark_skipped = getattr(self.db, "mark_document_skipped", None)
                    if mark_skipped:
                        mark_skipped(
                            dataset_id,
                            db_file_key,
                            message=str(file_err),
                            error_code="UNSUPPORTED_INDEXING_SOURCE",
                        )
                    else:
                        self.db.update_document_status(
                            dataset_id, db_file_key, "SKIPPED", 0, last_error=str(file_err)
                        )
                    _add_timing("db_sec", phase_start)

                except Exception as file_err:
                    support.logger.error(f"[PARSE] ERROR {file_key}: {file_err}", exc_info=True)
                    try:
                        with journal.lease():
                            decision = journal.pending() if journal_started else None
                            if not replacement_committed and (not decision or decision["phase"] == "staging"):
                                discard_staged(sync_qdrant, self.collection_name, staged_ids)
                                if journal_started:
                                    journal.finish()
                    except Exception as cleanup_err:
                        support.logger.error("[PARSE] cleanup failed %s: %s", file_key, cleanup_err)
                    phase_start = _t.time()
                    error_code, retryable, retry_after = support._parse_failure_policy(
                        file_err,
                        attempts=attempts,
                    )
                    mark_parse_error = getattr(self.db, "mark_document_parse_error", None)
                    if mark_parse_error:
                        mark_parse_error(
                            dataset_id,
                            db_file_key,
                            message=(
                                f"{error_code} [{type(file_err).__name__}]: "
                                f"{str(file_err) or 'exception without message'}"
                            ),
                            error_code=error_code,
                            retryable=retryable,
                            retry_after=retry_after,
                        )
                    else:
                        self.db.update_document_status(
                            dataset_id,
                            db_file_key,
                            "ERROR",
                            0,
                            last_error=str(file_err) or type(file_err).__name__,
                        )
                    _add_timing("db_sec", phase_start)
                    errors += 1
                    if journal.pending() is not None:
                        # Recovery must complete before the next file can write.
                        return 1
                return 0

            attempted = 0
            try:
                if intake_id is not None:
                    completed = native_core.dispatch_index_batch(self.db.db_path, intake_id, sources, process_one)
                    attempted = completed["attempted"]
                elif publisher is not None:
                    selection = [(db_key, external_sources.get(db_key, '')) for _, _, db_key in files_to_parse]
                    for offset in range(0, total, 500):
                        completed = native_core.dispatch_index_batch(self.db.db_path, None, selection[offset:offset + 500],
                            lambda index, attempts: process_one(offset + index, attempts), dataset_id=dataset_id)
                        attempted += completed["attempted"]
                        if completed["interrupted"]:
                            break
                else:
                    begin_attempt = getattr(self.db, "begin_document_attempt", None)
                    for index, (_, _, db_key) in enumerate(files_to_parse):
                        attempts = int(begin_attempt(dataset_id, db_key) or 1) if begin_attempt else 1
                        attempted += 1
                        if process_one(index, attempts):
                            break
            finally:
                if convert_pool is not None:
                    convert_pool.shutdown(wait=False, cancel_futures=True)

            phase_start = _t.time()
            self.db.update_dataset_chunk_count(dataset_id)
            if publisher is not None:
                remaining_pending = native_core.required("index_sources", request={
                    "database": str(support.Path(self.db.db_path).resolve()),
                    "dataset_id": dataset_id, "count_only": True,
                })["remaining_pending"]
            else:
                remaining_pending = len(self.db.get_pending_files(dataset_id))
            _add_timing("db_sec", phase_start)
            elapsed = _t.time() - t0
            timings = {key: round(value, 3) for key, value in timings.items()}
            support.logger.info(
                f"[PARSE] DONE: {total} файлов, {total_chunks} чанков, "
                f"{errors} ошибок за {elapsed:.0f}с, осталось pending={remaining_pending}, "
                f"timings={timings}"
            )
            return {
                "status":       "completed",
                "chunks":       total_chunks,
                "files_parsed": attempted,
                "files_skipped": max(0, total_all - internal_count),
                "remaining_pending": remaining_pending,
                "errors":       errors,
                "embedding_cache_hits": embedding_cache_hits,
                "embedded_chunks": embedded_chunks,
                "elapsed_sec":  round(elapsed, 1),
                "timings":      timings,
            }

        except Exception as e:
            support.logger.error(f"[PARSE] FATAL: {e}", exc_info=True)
            return {"status": "failed", "error": str(e)}
        finally:
            if sync_qdrant is not None:
                close = getattr(sync_qdrant, "close", None)
                if close is not None:
                    try: close()
                    except Exception: support.logger.warning("Qdrant pool close failed", exc_info=True)


    def _convert_file(
        self,file_path,data_dir,file_key,dataset_id,md_parser,splitter,timings,allow_ocr=True,
    ):
        from backend.native_document_conversion import prepare as prepare_native
        converted=prepare_native(file_path,data_dir,file_key,dataset_id,splitter,timings)
        if converted is not None:
            return converted
        from backend import format_process
        if format_process.enabled():
            try:
                return format_process.prepare(file_path,data_dir,file_key,dataset_id,splitter,timings,allow_ocr)
            except format_process.FormatProcessError as error:
                if error.code=='FORMAT_UNSUPPORTED_SOURCE':
                    raise support.UnsupportedIndexingSourceError(support._raw_cad_bim_error(file_path)) from error
                raise
        return QdrantIngestion._convert_file_inline(
            self,file_path,data_dir,file_key,dataset_id,md_parser,splitter,timings,allow_ocr)

    def _convert_file_inline(
        self,
        file_path: support.Path,
        data_dir: support.Path,
        file_key: str,
        dataset_id: str,
        md_parser,
        splitter,
        timings: dict,
        allow_ocr: bool = True,
    ):
        """W1.4: стадия конвертации (route + nodes), вынесена для префетча в фоне.

        allow_ocr=False (префетч): OCR-файлы не конвертируем в фоне — возвращаем
        (route, None), основной поток выполнит конвертацию синхронно.
        """
        import time as _t

        def _add_timing(key: str, started: float) -> None:
            timings[key] = timings.get(key, 0.0) + (_t.time() - started)

        phase_start = _t.time()
        route = support.route_document(file_path)
        _add_timing("route_sec", phase_start)
        support.logger.info(
            "[DOC_ROUTE] %s domain=%s dataset=%s type=%s content=%s complexity=%s pipeline=%s",
            file_key,
            route.domain,
            route.dataset_name,
            route.doc_type,
            route.content_type,
            route.complexity,
            route.pipeline,
        )

        if support._is_raw_cad_bim_source(file_path, route):
            raise support.UnsupportedIndexingSourceError(support._raw_cad_bim_error(file_path))

        if route.pipeline == "markdown_needs_ocr" and not allow_ocr:
            return route, None

        if route.doc_type == "EMAIL":
            file_nodes = self._sync_mail_nodes(
                file_path, data_dir, file_key, dataset_id, splitter, route, timings
            )
        elif route.pipeline == "parquet":
            try:
                file_nodes = self._sync_table_nodes(file_path, data_dir, file_key, dataset_id, route, timings)
                if not file_nodes and file_path.suffix.lower() in {'.csv', '.xlsx', '.xlsm', '.xls'}:
                    # Unknown column names must not erase readable source rows.
                    raise RuntimeError("No structured table nodes; read original sheet as markdown")
            except Exception as table_err:
                support.logger.warning(
                    "[PARQUET] fallback to markdown for %s: %s",
                    file_key,
                    table_err,
                )
                file_nodes = self._sync_markdown_nodes(
                    file_path, file_key, dataset_id, md_parser, splitter, route, timings
                )
        elif route.pipeline in ("markdown_pdf_tables", "markdown_needs_ocr"):
            file_nodes = self._sync_markdown_nodes(
                file_path, file_key, dataset_id, md_parser, splitter, route, timings
            )
            if (
                route.pipeline == "markdown_pdf_tables"
                and support.os.getenv("PDF_TABLE_EXTRACTION_ENABLED", "false").lower() == "true"
            ):
                try:
                    file_nodes.extend(self._sync_table_nodes(file_path, data_dir, file_key, dataset_id, route, timings))
                except Exception as table_err:
                    support.logger.warning(
                        "[PDF_TABLE] table extraction skipped for %s: %s",
                        file_key,
                        table_err,
                    )
        else:
            file_nodes = self._sync_markdown_nodes(
                file_path, file_key, dataset_id, md_parser, splitter, route, timings
            )
            if QdrantNodes._docx_table_extraction_enabled(file_path, route):
                try:
                    file_nodes.extend(self._sync_table_nodes(file_path, data_dir, file_key, dataset_id, route, timings))
                except Exception as table_err:
                    support.logger.warning(
                        "[DOCX_TABLE] table extraction skipped for %s: %s",
                        file_key,
                        table_err,
                    )
        return route, file_nodes

