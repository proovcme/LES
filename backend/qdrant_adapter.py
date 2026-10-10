"""Public Qdrant facade and catalog operations."""
from __future__ import annotations
from backend import qdrant_support as support
from backend.qdrant_support import (
    ALLOW_UNBOUNDED_PARSE,
    Any,
    CHUNK_HASH_CACHE,
    Chunk,
    DatasetInfo,
    Dict,
    Document,
    DocumentRoute,
    EMBED_BATCH,
    EmbedClient,
    EmbeddingContractError,
    FINAL_MIN_CHUNK,
    FuturesTimeoutError,
    List,
    MIN_CHUNK,
    Mapping,
    MarkdownNodeParser,
    MetaDB,
    Optional,
    PARSE_FILE_TIMEOUT,
    PARSE_PREFETCH,
    PDF_PAGE_NODE_SUFFIXES,
    ParseFailureDisposition,
    Path,
    RAGBackend,
    RAG_CHUNK_OVERLAP,
    RAG_CHUNK_SIZE,
    RAW_CAD_BIM_SUFFIXES,
    SentenceSplitter,
    StructureAwareSplitter,
    TABLE_ROW_INDEX_MAX_CHUNKS,
    TableNormalizer,
    ThreadPoolExecutor,
    UPSERT_BATCH,
    UnsupportedIndexingSourceError,
    VERIFY_POINTS_EVERY,
    _BASE64_RUN_RE,
    _CONTROL_CHARS_RE,
    _DATA_URI_RE,
    _MD_HEADING_RE,
    _NUM_HEADING_RE,
    _TRUE_ENV_VALUES,
    _apply_collection_count_health,
    _apply_context_metadata_to_nodes,
    _can_adopt_missing_contract,
    _classify_parse_failure,
    _compact_text,
    _content_hash,
    _dense_vector_name,
    _embedding_cache_descriptor,
    _embedding_cache_fingerprint,
    _field,
    _is_raw_cad_bim_source,
    _largest_budget_prefix,
    _legacy_navigation_count_candidate,
    _named_collection_layout,
    _parse_failure_policy,
    _pdf_page_node_max_chars,
    _pdf_page_node_overlap_chars,
    _pdf_page_nodes_enabled,
    _pdf_page_passport_enabled,
    _point_fingerprint_coverage_ready,
    _prepare_named_sparse_nodes,
    _qdrant_schema_mode,
    _raw_cad_bim_error,
    _sanitize_embedding_text,
    _section_heading,
    _section_heading_info,
    _sha256_file,
    _sparse_vector_name,
    _split_to_embedding_budget,
    asyncio,
    build_mail_vector_profile,
    chunk_payload_typing,
    chunking_config,
    convert_to_markdown_for_indexing,
    current_dataset_revision_id,
    dataclass,
    deterministic_mail_node_id,
    hashlib,
    index_contract_status,
    logger,
    logging,
    models,
    mutable_path,
    os,
    payload_index_ensure_enabled,
    point_embedding_descriptor,
    point_embedding_fingerprint,
    qdrant_client,
    qdrant_client_options,
    rag_chunk_overlap,
    rag_chunk_size,
    rag_collection_name,
    rag_vector_size,
    re,
    route_document,
    shutil,
    sys,
    time,
    uuid,
    write_index_contract,
)
from backend.qdrant_collection import QdrantCollection
from backend.qdrant_ingestion import QdrantIngestion
from backend.qdrant_integrity import QdrantIntegrity
from backend.qdrant_nodes import QdrantNodes
from backend.qdrant_retrieval import QdrantRetrieval


class QdrantLlamaIndexAdapter(QdrantCollection, QdrantIngestion, QdrantIntegrity, QdrantNodes, QdrantRetrieval, support.RAGBackend):
    @classmethod
    def for_catalog_maintenance(
        cls,
        *,
        qdrant_url: str,
        collection_name: str,
        meta_db_path: str | support.Path,
        sync_qdrant_client: support.Any | None = None,
    ) -> "QdrantLlamaIndexAdapter":
        """Create a narrow maintenance view without models, storage or runtime startup."""
        adapter = cls.__new__(cls)
        adapter.qdrant_url = str(qdrant_url)
        adapter.collection_name = str(collection_name)
        adapter.db = support.MetaDB(str(meta_db_path))
        adapter._catalog_sync_qdrant = sync_qdrant_client
        return adapter

    def __init__(
        self,
        qdrant_url:       str,
        mlx_url:       str,
        embed_model_name: str,
        content_dir:      str | support.Path | None = None,
    ):
        self.content_dir     = support.Path(content_dir) if content_dir is not None else support.mutable_path("storage/datasets")
        self.content_dir.mkdir(parents=True, exist_ok=True)
        self.db              = support.MetaDB()
        self.db.ensure_system_datasets()
        try:
            repair_limit = max(0, int(support.os.getenv("RAG_BOUNDED_REPAIR_MAX_FILES", "50")))
        except ValueError:
            repair_limit = 50
        repair = self.db.requeue_repairable_errors(max_files=repair_limit)
        if repair["repaired_files"]:
            support.logger.warning(
                "[INIT] Bounded indexing repair requeued %s/%s eligible files",
                repair["repaired_files"],
                repair["eligible_files"],
            )
        recovered = self.db.recover_interrupted_parsing()
        if recovered:
            support.logger.info("[INIT] Recovered %s interrupted parsing dataset(s)", recovered)
        # check_compatibility=False: пропустить версии-чек клиент↔сервер. Иначе на Windows он
        # вис («Failed to obtain server version») и держал /api/health/ретрив десятками секунд,
        # когда Qdrant недоступен (#2). Skip → операции фейлятся быстро, версия не блокирует старт.
        self.aclient         = support.qdrant_client.AsyncQdrantClient(
            url=qdrant_url, timeout=60.0, check_compatibility=False, **support.qdrant_client_options(qdrant_url))
        self.qdrant_url      = qdrant_url
        if (support.sys.platform == "win32" or support.os.getenv("EMBED_BACKEND") == "ollama") and ("8080" in mlx_url or not mlx_url):
            mlx_url = "http://127.0.0.1:11434"
        embed_backend = "ollama" if ("11434" in mlx_url or support.sys.platform == "win32" or support.os.getenv("EMBED_BACKEND") == "ollama") else None
        from backend.product_edition import is_light
        connection_mode = "active" if is_light() else "legacy"
        self.embed           = support.EmbedClient(mlx_url, model=embed_model_name.replace(":latest", ""), backend=embed_backend, connection_mode=connection_mode)
        # Отдельный эмбеддер для ПАРСА (опц.): EMBED_URL_PARSE → парс-эмбеддинги уходят на
        # ВТОРОЙ инстанс, не голодая чат-эмбеддинг на основном :8080 во время индексации.
        # Дефолт = основной URL (ноль изменений, пока env не задан). Активация: поднять второй
        # MLX-эмбеддер на альт-порту + EMBED_URL_PARSE=http://127.0.0.1:<порт>.
        _parse_url = support.os.getenv("EMBED_URL_PARSE", "").strip() or mlx_url
        parse_backend = "ollama" if ("11434" in _parse_url or support.sys.platform == "win32" or support.os.getenv("EMBED_BACKEND") == "ollama") else None
        self.embed_parse     = support.EmbedClient(_parse_url, model=embed_model_name.replace(":latest", ""), backend=parse_backend, connection_mode=connection_mode)
        if _parse_url != mlx_url:
            support.logger.info("[INIT] парс-эмбеддер на отдельном инстансе: %s", _parse_url)
        self.collection_name = support.rag_collection_name()
        self.vector_size     = support.rag_vector_size()
        self._collection_ready = False
        self._collection_lock  = support.asyncio.Lock()
        self._payload_index_task: support.asyncio.Task | None = None

    async def list_datasets(self) -> support.List[support.DatasetInfo]:
        return self.db.list_datasets()

    async def create_dataset(self, name: str) -> str:
        return self.db.create_dataset(name)

    async def set_dataset_sensitivity(self, dataset_id: str, sensitivity: str) -> None:
        self.db.set_dataset_sensitivity(dataset_id, sensitivity)

    async def set_dataset_group(self, dataset_id: str, group_name: str) -> None:
        self.db.set_dataset_group(dataset_id, group_name)

    async def set_dataset_name(self, dataset_id: str, name: str) -> None:
        self.db.set_dataset_name(dataset_id, name)

    async def upload_file(self, dataset_id: str, file_path: support.Path, relative_path: support.Optional[str] = None) -> str:
        dest_dir  = self.content_dir / dataset_id
        dest_dir.mkdir(parents=True, exist_ok=True)
        rel_name = relative_path or file_path.name
        rel_path = support.Path(rel_name)
        if rel_path.is_absolute() or ".." in rel_path.parts:
            raise ValueError(f"unsafe relative path: {rel_name}")
        dest_file = dest_dir / rel_path
        dest_file.parent.mkdir(parents=True, exist_ok=True)

        stat  = file_path.stat() if file_path.exists() else None
        mtime = stat.st_mtime if stat else 0.0
        size  = stat.st_size  if stat else 0

        if file_path.exists() and file_path != dest_file:
            await support.asyncio.to_thread(support.shutil.copy2, file_path, dest_file)

        doc_id, _, needs_reindex = self.db.add_document(
            dataset_id, rel_path.as_posix(), file_mtime=mtime, file_size=size
        )
        try:
            route_source = dest_file if dest_file.exists() else file_path
            route = support.route_document(route_source)
            if needs_reindex:
                self.db.update_document_status(dataset_id, rel_path.as_posix(), "PENDING", 0, route=route)
            else:
                self.db.update_document_route(dataset_id, rel_path.as_posix(), route)
        except Exception as error:
            support.logger.warning("[DOC_ROUTE] upload classification skipped for %s: %s", rel_path.as_posix(), error)
        return doc_id

    async def mark_document_error(self, dataset_id: str, document_id: str, error: str) -> None:
        await support.asyncio.to_thread(self.db.mark_document_error, dataset_id, document_id, error)

    async def mark_document_deferred(self, dataset_id: str, document_id: str, reason: str) -> None:
        await support.asyncio.to_thread(self.db.mark_document_deferred, dataset_id, document_id, reason)

    async def register_external_file(self, dataset_id: str, source_path: support.Path, file_name: str, *, force_reindex: bool = False) -> str:
        """Регистрирует внешний файл как источник БЕЗ копии в storage.

        Документ остаётся в своей папке; в storage/datasets/{id} попадают только
        производные (Parquet/_parquet). file_name — ключ дока (rel-путь под корнем),
        source_path — абсолютный путь, по которому _sync_parse прочитает файл.
        """
        rel = support.Path(file_name)
        if rel.is_absolute() or ".." in rel.parts:
            raise ValueError(f"unsafe file_name: {file_name}")
        # Каталог датасета нужен для производных (Parquet) и для прохода _sync_parse.
        (self.content_dir / dataset_id).mkdir(parents=True, exist_ok=True)

        src = support.Path(source_path)
        stat = src.stat() if src.exists() else None
        mtime = stat.st_mtime if stat else 0.0
        size = stat.st_size if stat else 0

        doc_id, _, needs_reindex = self.db.add_document(
            dataset_id, rel.as_posix(), file_mtime=mtime, file_size=size, source_path=str(src),
            **({"force_reindex": True} if force_reindex else {}),
        )
        try:
            route = support.route_document(src)
            if needs_reindex:
                self.db.update_document_status(dataset_id, rel.as_posix(), "PENDING", 0, route=route)
            else:
                self.db.update_document_route(dataset_id, rel.as_posix(), route)
        except Exception as error:
            support.logger.warning("[EXT_DOC] classification skipped for %s: %s", rel.as_posix(), error)
        return doc_id

