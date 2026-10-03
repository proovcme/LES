"""Dataset API assembly. Implementations live in focused routers and services."""
from fastapi import APIRouter
from proxy.routers.dataset_catalog import (
    _validated_dataset_name,
    audit_navigation_count_consistency,
    benchmark_dataset_context_profiles,
    configure_dataset_watch,
    create_dataset,
    dataset_context_profile,
    dataset_watch_status,
    delete_all_datasets,
    delete_dataset,
    get_catalog_consistency,
    get_rag_readiness,
    list_datasets,
    list_documents,
    list_sources,
    refresh_dataset_context_profile,
    repair_catalog_consistency,
    repair_navigation_count_consistency,
    set_dataset_group,
    set_dataset_name,
    set_dataset_sensitivity,
    update_dataset_kind,
    update_dataset_operator_guidance,
    warmup_dataset_context_profiles,
)
from proxy.routers.dataset_catalog import router as catalog_router
from proxy.routers.dataset_document_ops import (
    dataset_integrity,
    document_registry_build_endpoint,
    document_registry_endpoint,
    extract_body_dry_run,
    extract_body_write,
    extraction_status_endpoint,
    graph_full,
    graph_reference_edges,
    pdf_extract_run_endpoint,
    pdf_extract_status_endpoint,
    pdf_extract_summary_endpoint,
    reconcile_dataset_endpoint,
    repair_dataset,
    repair_dataset_integrity,
    table_registry_build_endpoint,
    table_registry_read_endpoint,
    table_registry_search_endpoint,
    table_registry_summary_endpoint,
    virtual_volume_endpoint,
)
from proxy.routers.dataset_document_ops import router as document_ops_router
from proxy.routers.dataset_watch import (
    _folder_watch_cache,
    _folder_watch_cache_key,
    _folder_watch_cache_lock,
    _folder_watch_inventory,
    _folder_watch_inventory_cached,
    _known_docs_inventory,
    _safe_source_root,
    _trim_folder_watch_status,
    build_folder_reindex_plan,
    build_folder_watch_status,
    clear_folder_watch_cache,
    folder_reindex_plan,
    folder_watch_scan,
    folder_watch_status,
    smart_plan,
    sync_smart,
)
from proxy.routers.dataset_watch import router as watch_router
from proxy.routers.dataset_external import (
    _count_dir_files,
    _delete_index_for_files,
    _discipline_hints,
    _document_role_hint,
    _external_dataset_diff,
    _external_dataset_docs,
    _external_intake_plan,
    _external_supported_files,
    _index_external_run,
    _index_external_run_safe,
    _mark_external_missing,
    _project_name_from_dataset,
    check_external_dataset,
    external_intake_plan,
    index_external,
    sync_external_dataset,
)
from proxy.routers.dataset_external import router as external_router
from proxy.routers.dataset_cloud import (
    _cloud_drive_sync_run,
    browse_external,
    cloud_drive_list,
    cloud_drive_sync,
    cloud_drives,
)
from proxy.routers.dataset_cloud import router as cloud_router
from proxy.routers.dataset_uploads import (
    _dataset_id_for_name,
    _ensure_chat_attach_dataset,
    _record_background_parse_error,
    _upload_intake_response,
    attach_chat_file,
    attach_chat_folder,
    create_chat_attachment,
    upload_file,
    upload_file_smart,
)
from proxy.routers.dataset_uploads import router as uploads_router
from proxy.routers.dataset_parse import (
    parse_dataset_batch,
    parse_scheduler,
    sync_folder,
)
from proxy.routers.dataset_parse import router as parse_router
from proxy.routers.dataset_search import (
    _chunk_payload,
    retrieve_debug,
    search,
)
from proxy.routers.dataset_search import router as search_router
from proxy.services.dataset_runtime import (
    DatasetRouterState,
    _table_exists,
    get_dataset_state,
    set_dataset_state,
)
from proxy.services.dataset_parse_service import (
    _dataset_name_for_id,
    _parse_progress_snapshot,
    _parse_result_ready_for_reader,
    _parse_with_job_progress,
    _pending_count_for_dataset,
    _priority_rank,
    _processed_from_parse_result,
    _schedule_reader_after_parse,
    active_parse_scheduler_job,
    assert_parse_admission,
    parse_memory_state,
    pending_parse_datasets,
    run_dataset_parse_drain,
    run_parse_scheduler,
)
from proxy.services.dataset_contracts import (
    ACTIVE_PARSE_SCHEDULER_STATUSES,
    ChatFolderRead,
    CloudDriveListRequest,
    CloudDriveSyncRequest,
    CreateDatasetRequest,
    DEFAULT_PARSE_BATCH_LIMIT,
    DEFAULT_PARSE_DRAIN_MAX_BATCHES,
    DEFAULT_PARSE_SCHEDULER_BATCH_LIMIT,
    DEFAULT_PARSE_SCHEDULER_MAX_BATCHES,
    DatasetGroupPayload,
    DatasetGuidanceRequest,
    DatasetKindRequest,
    DatasetNamePayload,
    DatasetProfileWarmupRequest,
    DatasetWatchRequest,
    EXTERNAL_SERVICE_FILENAMES,
    ExternalDatasetSyncRequest,
    ExternalIntakePlanRequest,
    FOLDER_WATCH_CACHE_SAMPLE_LIMIT,
    FOLDER_WATCH_CACHE_TTL_SEC,
    FolderWatchRequest,
    IndexExternalRequest,
    PARSE_MAX_SWAP_PCT,
    PARSE_MIN_FREE_GB,
    PARSE_POST_MAX_SWAP_PCT,
    ParseSchedulerRequest,
    RetrievalDebugRequest,
    SearchRequest,
    SmartSyncRequest,
    UUID_RE,
    _CHAT_ATTACH_DATASET_NAME,
    _EXTRACT_STORAGE_ROOT,
    _PARSE_STAGE_LABELS,
    _TRUE_ENV_VALUES,
)
from proxy.routers.dataset_search import search_router as query_router
from proxy.routers.dataset_uploads import search_router as attachment_router
from proxy.security import require_admin, require_root_admin, require_user

router = APIRouter()
search_router = APIRouter()
search_router.include_router(query_router)
search_router.include_router(attachment_router)
router.include_router(catalog_router)
router.include_router(document_ops_router)
router.include_router(watch_router)
router.include_router(external_router)
router.include_router(cloud_router)
router.include_router(uploads_router)
router.include_router(parse_router)
router.include_router(search_router)
