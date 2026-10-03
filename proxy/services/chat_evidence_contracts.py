"""Explicit request, dependency and response contracts for evidence execution."""
from __future__ import annotations
import logging
from dataclasses import dataclass, field
from typing import Any, Callable




@dataclass(frozen=True)
class _SkippedRetrievalQuality:
    status: str = "skipped"
    top_score: float = 0.0


@dataclass(frozen=True)
class _SkippedRetrievalTrace:
    status: str = "skipped"
    error_code: str = ""


@dataclass(frozen=True)
class _SkippedDocumentRetrieval:
    chunks: tuple[Any, ...] = ()
    trace: _SkippedRetrievalTrace = _SkippedRetrievalTrace()
    quality: _SkippedRetrievalQuality = _SkippedRetrievalQuality()

    def payload(self) -> dict[str, Any]:
        return {
            "schema": "retrieval_trace_v1",
            "status": "skipped",
            "reason": "scope_none",
            "quality": {"status": "skipped", "top_score": 0.0},
        }


@dataclass(frozen=True)
class EvidenceRequestContext:
    req: Any
    dataset_ids: list[str]
    effective_dataset_filter: str
    resolved_dataset_names: list[str]
    dataset_name_by_id: dict[str, str]
    query_route_payload: dict[str, Any]
    target_doc_filter: list[str]
    target_file_ref: dict[str, Any] | None
    topic_doc_filter: list[str]
    topic_retrieval_plan: dict[str, Any] | None
    inventory_requested: bool
    study_requested: bool
    memory_block: str
    session_block: str
    class_suggestions: list[dict[str, Any]]
    use_semantic_cache: bool
    use_validation: bool
    validation_skip_reason: str
    route: Any
    table_result: Any
    request_started_at: float
    profile_snapshot: dict[str, Any] = field(default_factory=dict)
    scope_resolution: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class EvidenceRuntimeDeps:
    state: Any
    rag_backend: Any
    cache: Any
    cache_embedding: Any
    cache_marker: str
    cache_scope: str
    augment_model_tool_args: Callable
    compact_tool_result_for_prompt: Callable
    dataset_ids_from_chunks: Callable
    dataset_sensitivities: Callable
    env_bool: Callable
    env_float: Callable
    env_int: Callable
    expand_context_windows: Callable
    format_tool_results_for_model: Callable
    generation_token_budget: Callable
    local_context_budget: Callable
    names_for_dataset_ids: Callable
    parse_model_tool_calls: Callable
    prepare_notebook_reader_memory: Callable
    record_cloud_cost: Callable
    retrieve_chat_chunks: Callable
    source_excerpts: Callable
    model_connection_resolver: Callable | None = None
    model_connection_transport: Callable | None = None


@dataclass(frozen=True)
class ResponseBoundary:
    save_chat_history: Callable
    token_sink: Callable | None
    version_stamp: Callable
