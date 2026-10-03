from __future__ import annotations
"""Qdrant indexing and retrieval with explicit dense/sparse contracts.

Shared Qdrant contracts, node normalization and dependency configuration.
Lifecycle, ingestion, integrity and retrieval live in focused adapter modules.
"""
from backend.structure_splitter import StructureAwareSplitter
from backend.embedding_client import EmbedClient
from backend.document_catalog import MetaDB
import asyncio
from backend.light_qdrant_connection import qdrant_client_options
import hashlib
import logging
import os
import re
import shutil
import sys
import time
import uuid
from concurrent.futures import ThreadPoolExecutor, TimeoutError as FuturesTimeoutError
from dataclasses import dataclass
from pathlib import Path
from collections.abc import Mapping
from typing import Any, Dict, List, Optional

import qdrant_client
from llama_index.core.node_parser import MarkdownNodeParser, SentenceSplitter
from llama_index.core.schema import Document
from qdrant_client import models

from backend.runtime_paths import mutable_path

from .converter import convert_to_markdown_for_indexing
from .document_router import DocumentRoute, route_document
from .interface import Chunk, DatasetInfo, EmbeddingContractError, RAGBackend
from .mail_profile import build_mail_vector_profile, deterministic_mail_node_id
from .parquet_writer import TableNormalizer
from proxy.services.dataset_memory_service import chunk_payload_typing, current_dataset_revision_id
from .rag_config import chunking_config, index_contract_status, rag_chunk_overlap, rag_chunk_size, rag_collection_name, rag_vector_size, point_embedding_descriptor, point_embedding_fingerprint, write_index_contract

logger = logging.getLogger(__name__)


def payload_index_ensure_enabled() -> bool:
    """Whether startup may submit idempotent Qdrant payload-index mutations."""
    if os.getenv("LES_STARTUP_BACKGROUND_MUTATIONS", "true").lower() not in {
        "1", "true", "yes", "on"
    }:
        return False
    return os.getenv("LES_RAG_PAYLOAD_INDEX_ENSURE", "true").lower() in {
        "1", "true", "yes", "on"
    }


RAW_CAD_BIM_SUFFIXES = {".dwg", ".dxf", ".rvt", ".rfa", ".ifc", ".ifczip", ".nwc"}
PDF_PAGE_NODE_SUFFIXES = {".pdf", ".p7m"}


def _pdf_page_nodes_enabled(file_path: Path, route: DocumentRoute | None = None) -> bool:
    if file_path.suffix.lower() not in PDF_PAGE_NODE_SUFFIXES:
        return False
    if os.getenv("RAG_PDF_PAGE_NODES_ENABLED", "true").lower() not in ("1", "true", "yes", "on"):
        return False
    return True


def _pdf_page_passport_enabled(file_path: Path) -> bool:
    return (
        file_path.suffix.lower() == ".pdf"
        and os.getenv("RAG_PDF_PAGE_PASSPORT_ENABLED", "true").lower() in ("1", "true", "yes", "on")
    )


def _pdf_page_node_max_chars() -> int:
    try:
        return max(800, int(os.getenv("RAG_PDF_PAGE_NODE_MAX_CHARS", "1800")))
    except ValueError:
        return 1800


def _pdf_page_node_overlap_chars() -> int:
    try:
        return max(0, int(os.getenv("RAG_PDF_PAGE_NODE_OVERLAP_CHARS", "150")))
    except ValueError:
        return 150


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class UnsupportedIndexingSourceError(RuntimeError):
    """Raised when intake accepted a source that needs a typed converter first."""


@dataclass(frozen=True)
class ParseFailureDisposition:
    """Stable, UI-safe classification of one indexing failure."""

    error_code: str
    disposition: str
    retryable: bool
    retry_after: float
    attempts: int
    max_attempts: int


def _classify_parse_failure(error: Exception, *, attempts: int) -> ParseFailureDisposition:
    """Classify a failure without leaking exception text as product state."""
    attempts = max(0, int(attempts or 0))
    try:
        max_attempts = max(1, int(os.getenv("RAG_PARSE_MAX_ATTEMPTS", "4")))
    except ValueError:
        max_attempts = 4

    if isinstance(error, UnsupportedIndexingSourceError):
        return ParseFailureDisposition(
            error_code="UNSUPPORTED_INDEXING_SOURCE",
            disposition="skipped",
            retryable=False,
            retry_after=0.0,
            attempts=attempts,
            max_attempts=max_attempts,
        )

    message = str(error or "").casefold()
    transient_codes = (
        ("missing prevalidated sparse vector", "SPARSE_VECTOR_PREVALIDATION_MISSING"),
        ("qdrant point count mismatch", "QDRANT_POINT_COUNT_MISMATCH"),
        ("embedding count mismatch", "EMBEDDING_COUNT_MISMATCH"),
        ("timed out", "PARSE_TIMEOUT"),
        ("timeout", "PARSE_TIMEOUT"),
        ("temporarily unavailable", "DEPENDENCY_UNAVAILABLE"),
        ("all connection attempts failed", "DEPENDENCY_UNAVAILABLE"),
        ("connection", "DEPENDENCY_UNAVAILABLE"),
    )
    error_code = next((code for marker, code in transient_codes if marker in message), "PARSE_FAILED")
    transient = error_code != "PARSE_FAILED"
    retryable = transient and attempts < max_attempts
    delay = min(300.0, 5.0 * (2 ** max(0, attempts - 1))) if retryable else 0.0
    return ParseFailureDisposition(
        error_code=error_code,
        disposition="retryable" if retryable else "terminal",
        retryable=retryable,
        retry_after=time.time() + delay if retryable else 0.0,
        attempts=attempts,
        max_attempts=max_attempts,
    )


def _parse_failure_policy(error: Exception, *, attempts: int) -> tuple[str, bool, float]:
    """Return stable code, bounded retry decision and absolute retry time."""
    classified = _classify_parse_failure(error, attempts=attempts)
    return classified.error_code, classified.retryable, classified.retry_after


def _point_fingerprint_coverage_ready(*, points: int, matching: int) -> bool:
    return points == 0 or (matching == points and matching > 0)


def _legacy_navigation_count_candidate(
    *,
    expected: int,
    actual: int,
    navigation: int,
    dense: int,
    sparse: int,
    lexical_matches: bool,
    source_actual: int | None = None,
    source_navigation: int | None = None,
) -> bool:
    """Allow metadata-only repair only when every extra point is explicit navigation."""
    navigation_delta = navigation
    source_matches = True
    if source_actual is not None:
        source_matches = source_actual == expected
        navigation_delta = navigation - max(0, int(source_navigation or 0))
    return bool(
        expected > 0
        and actual > expected
        and source_matches
        and navigation_delta > 0
        and actual - expected == navigation_delta
        and dense == actual
        and sparse == actual
        and lexical_matches
    )


def _is_raw_cad_bim_source(file_path: Path, route: DocumentRoute | None) -> bool:
    suffix = file_path.suffix.lower()
    return suffix in RAW_CAD_BIM_SUFFIXES and (
        route is None
        or route.content_type == "cad_bim"
        or route.pipeline == "json_graph_projection"
        or route.doc_type == "CAD_BIM"
    )


def _raw_cad_bim_error(file_path: Path) -> str:
    suffix = file_path.suffix.lower() or "raw"
    return (
        f"raw CAD/BIM source unsupported by text RAG indexing ({suffix}); "
        "export/import it as canonical CAD/BIM JSON/JSONL projection before indexing"
    )


EMBED_BATCH  = int(os.getenv("RAG_EMBED_BATCH", "16"))      # чанков за один запрос к MLX embeddings
MIN_CHUNK    = int(os.getenv("RAG_MIN_CHUNK_CHARS", "100"))  # W2.5: <100 симв — шум («Приложение», «А»), не индексируем
FINAL_MIN_CHUNK = int(os.getenv("RAG_FINAL_MIN_CHUNK_CHARS", "20"))


def _apply_collection_count_health(
    snapshot: Dict[str, Any], *, physical_points: int
) -> None:
    """Compare one physical collection only with catalog rows owned by it.

    Module-owned system datasets can use typed stores or dedicated indexes.  They
    remain visible in global MetaDB totals but are not evidence that the active
    general collection is incomplete.
    """
    datasets = snapshot.get("datasets") or []
    comparable = sum(
        int(item.get("chunks") or 0)
        for item in datasets
        if str(item.get("dataset_scope") or "user").casefold() != "system"
    )
    excluded = sum(
        int(item.get("chunks") or 0)
        for item in datasets
        if str(item.get("dataset_scope") or "user").casefold() == "system"
    )
    qdrant = snapshot.setdefault("qdrant", {})
    physical = int(physical_points)
    legacy_system_points = excluded if excluded and physical == comparable + excluded else 0
    matches = physical == comparable or legacy_system_points > 0
    qdrant["count_comparison_scope"] = "active_user_catalog"
    qdrant["catalog_comparable_chunks"] = comparable
    qdrant["catalog_excluded_system_chunks"] = excluded
    qdrant["legacy_system_points"] = legacy_system_points
    qdrant["points_match_sqlite_chunks"] = matches
    if not matches:
        qdrant["mismatch"] = {
            "catalog_comparable_chunks": comparable,
            "qdrant_points": physical,
        }
        snapshot["status"] = "degraded"
UPSERT_BATCH = int(os.getenv("RAG_UPSERT_BATCH", "100"))    # точек за один upsert в Qdrant
TABLE_ROW_INDEX_MAX_CHUNKS = int(os.getenv("RAG_TABLE_ROW_INDEX_MAX_CHUNKS", "600"))
VERIFY_POINTS_EVERY = max(1, int(os.getenv("RAG_VERIFY_POINTS_EVERY", "1")))  # P0: exact-count каждый файл by default
# W1.4: конвейер — конвертация следующего файла параллельно с эмбеддингом текущего,
# per-file таймаут конвертации (зависший файл помечается ERROR, индексация продолжается).
PARSE_PREFETCH = os.getenv("RAG_PARSE_PREFETCH", "true").lower() == "true"
PARSE_FILE_TIMEOUT = float(os.getenv("RAG_PARSE_FILE_TIMEOUT_SEC", "1800"))
CHUNK_HASH_CACHE = os.getenv("RAG_CHUNK_HASH_CACHE", "true").lower() in {"1", "true", "yes", "on"}
RAG_CHUNK_SIZE = rag_chunk_size()
RAG_CHUNK_OVERLAP = rag_chunk_overlap()
ALLOW_UNBOUNDED_PARSE = "ALLOW_UNBOUNDED_PARSE"
_TRUE_ENV_VALUES = {"1", "true", "yes", "on"}


def _content_hash(text: str) -> str:
    return hashlib.sha1(text.encode("utf-8", errors="ignore")).hexdigest()


def _embedding_cache_descriptor() -> dict[str, str]:
    return point_embedding_descriptor()


def _embedding_cache_fingerprint(descriptor: dict[str, str] | None = None) -> str:
    return point_embedding_fingerprint(descriptor)


def _qdrant_schema_mode() -> str:
    return "named"


def _dense_vector_name() -> str:
    return os.getenv("RAG_DENSE_VECTOR_NAME", "dense").strip() or "dense"


def _sparse_vector_name() -> str:
    from backend.inference.bm25_sparse import SPARSE_VECTOR_NAME

    return os.getenv("RAG_SPARSE_VECTOR_NAME", SPARSE_VECTOR_NAME).strip() or SPARSE_VECTOR_NAME


def _field(value: Any, name: str, default: Any = None) -> Any:
    if isinstance(value, Mapping):
        return value.get(name, default)
    return getattr(value, name, default)


def _named_collection_layout(info: Any, *, vector_size: int) -> tuple[bool, int]:
    """Return named dense+sparse compatibility and point count for a collection."""
    config = _field(info, "config", {})
    params = _field(config, "params", {})
    vectors = _field(params, "vectors", {})
    sparse_vectors = _field(params, "sparse_vectors", {})
    points_count = int(_field(info, "points_count", 0) or 0)
    if not isinstance(vectors, Mapping) or not isinstance(sparse_vectors, Mapping):
        return False, points_count
    dense = vectors.get(_dense_vector_name())
    dense_size = int(_field(dense, "size", 0) or 0) if dense is not None else 0
    compatible = dense_size == vector_size and _sparse_vector_name() in sparse_vectors
    return compatible, points_count


def _can_adopt_missing_contract(*, points_count: int, matching_fingerprint_count: int) -> bool:
    """A missing sidecar is safe to recreate only from a provably canonical collection."""
    return points_count == 0 or matching_fingerprint_count == points_count


_MD_HEADING_RE = re.compile(r"^(#{1,6})\s+(.{2,160})$")
_NUM_HEADING_RE = re.compile(r"^(\d+(?:\.\d+){0,4})[.\s]+([А-ЯЁA-Z].{1,150})$")
_DATA_URI_RE = re.compile(
    r"data:[^\s;,]{1,120}(?:;[^\s,]{1,80})*;base64,[A-Za-z0-9+/=\s]{128,}",
    re.IGNORECASE,
)
_BASE64_RUN_RE = re.compile(r"(?<![A-Za-z0-9+/=])[A-Za-z0-9+/]{256,}={0,2}(?![A-Za-z0-9+/=])")
_CONTROL_CHARS_RE = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")


def _sanitize_embedding_text(text: str) -> tuple[str, dict[str, Any]]:
    """Remove transport/binary payloads before they can become evidence.

    The gate is deliberately format-agnostic and runs after every converter, so
    mixed text+base64 chunks cannot bypass a parser-specific check.
    """
    raw = str(text or "")
    data_uri_count = len(_DATA_URI_RE.findall(raw))
    clean = _DATA_URI_RE.sub(" [binary attachment removed] ", raw)
    base64_count = len(_BASE64_RUN_RE.findall(clean))
    clean = _BASE64_RUN_RE.sub(" [binary payload removed] ", clean)
    control_count = len(_CONTROL_CHARS_RE.findall(clean))
    clean = _CONTROL_CHARS_RE.sub(" ", clean)
    clean = re.sub(r"[ \t]{3,}", "  ", clean)
    clean = re.sub(r"\n{4,}", "\n\n\n", clean).strip()
    return clean, {
        "data_uri_removed": data_uri_count,
        "base64_runs_removed": base64_count,
        "control_chars_removed": control_count,
        "sanitized": bool(data_uri_count or base64_count or control_count),
    }


def _largest_budget_prefix(text: str, *, budget: int, len_fn) -> int:
    """Largest non-empty character prefix whose real token length fits budget."""
    low, high = 1, len(text)
    best = 0
    while low <= high:
        mid = (low + high) // 2
        if len_fn(text[:mid]) <= budget:
            best = mid
            low = mid + 1
        else:
            high = mid - 1
    if best <= 0:
        return 1
    # Prefer a semantic boundary without throwing away more than 20% of budget.
    floor = max(1, int(best * 0.8))
    candidates = [text.rfind("\n", floor, best), text.rfind(" ", floor, best)]
    boundary = max(candidates)
    return boundary if boundary >= floor else best


def _split_to_embedding_budget(text: str, *, budget: int, len_fn) -> list[str]:
    clean = str(text or "").strip()
    if not clean:
        return []
    if len_fn(clean) <= budget:
        return [clean]
    parts: list[str] = []
    remaining = clean
    while remaining:
        if len_fn(remaining) <= budget:
            parts.append(remaining.strip())
            break
        cut = _largest_budget_prefix(remaining, budget=budget, len_fn=len_fn)
        part = remaining[:cut].strip()
        if part:
            parts.append(part)
        remaining = remaining[cut:].strip()
    return [part for part in parts if part]


def _section_heading_info(text: str) -> tuple[str, int]:
    """W2.5: (заголовок, уровень). Уровень: # → 1..6; «5.2.1 Текст» → глубина номера; 0 — нет."""
    for line in text.splitlines()[:6]:
        line = line.strip()
        if not line:
            continue
        md = _MD_HEADING_RE.match(line)
        if md:
            return md.group(2).strip(), len(md.group(1))
        num = _NUM_HEADING_RE.match(line)
        if num:
            return f"{num.group(1)} {num.group(2).strip()}", num.group(1).count(".") + 1
    return "", 0


def _section_heading(text: str) -> str:
    heading, _ = _section_heading_info(text)
    if heading:
        return heading
    # Старое поведение как fallback: первая осмысленная строка.
    for line in text.splitlines():
        line = line.strip(" #\t")
        if 4 <= len(line) <= 160:
            return line
    return ""


def _compact_text(text: str, limit: int = 1200) -> str:
    compact = re.sub(r"\s+", " ", text).strip()
    if len(compact) <= limit:
        return compact
    return compact[: limit - 1].rstrip() + "…"


def _apply_context_metadata_to_nodes(file_nodes: list[dict], dataset_id: str, file_key: str) -> None:
    if not file_nodes:
        return

    grouped: dict[str, list[int]] = {}
    try:
        window_size = max(1, int(os.getenv("RAG_PARENT_WINDOW_CHUNKS", "4")))
    except ValueError:
        window_size = 4
    from backend.rag_hierarchy import evidence_payload, navigation_payload

    last_heading = ""
    last_level = 0
    heading_stack: list[tuple[int, str, str]] = []
    navigation_nodes: dict[str, dict] = {}
    dataset_revision = current_dataset_revision_id(dataset_id)
    for chunk_ord, file_node in enumerate(file_nodes):
        payload = file_node.setdefault("payload", {})
        payload.setdefault("dataset_id", dataset_id)
        payload.setdefault("file_name", file_key)
        if dataset_revision:
            payload.setdefault("dataset_revision", dataset_revision)
        try:
            payload.update(chunk_payload_typing(file_key, payload, payload))
        except Exception:
            pass
        text = str(file_node.get("text") or "")
        payload.setdefault("chunk_ord", chunk_ord)
        payload.setdefault("child_ord", chunk_ord)
        payload.setdefault("content_hash", _content_hash(text))
        # W2.5: настоящий заголовок (markdown/нумерованный) с уровнем; чанки-продолжения
        # наследуют последний найденный заголовок раздела.
        heading, level = _section_heading_info(text)
        if heading:
            last_heading, last_level = heading, level
            while heading_stack and heading_stack[-1][0] >= level:
                heading_stack.pop()
            identity = "/".join([item[1] for item in heading_stack] + [heading])
            nav_meta = navigation_payload(
                dataset_id=dataset_id,
                document_id=file_key,
                identity=identity,
                title=heading,
                ancestor_ids=[item[2] for item in heading_stack],
                depth=len(heading_stack) + 1,
            )
            heading_stack.append((level, heading, nav_meta["node_id"]))
            navigation_nodes.setdefault(
                nav_meta["node_id"],
                {
                    "text": heading,
                    "payload": {
                        **nav_meta,
                        "dataset_id": dataset_id,
                        "file_name": file_key,
                        "section_heading": heading,
                        "heading_level": level,
                        "content_hash": _content_hash(
                            f"navigation:{dataset_id}:{file_key}:{identity}"
                        ),
                    },
                },
            )
            payload.setdefault("section_heading", heading)
            payload.setdefault("heading_level", level)
        elif last_heading:
            payload.setdefault("section_heading", last_heading)
            payload.setdefault("heading_level", last_level)
            payload.setdefault("heading_inherited", True)
        else:
            payload.setdefault("section_heading", _section_heading(text))
        hierarchy = evidence_payload(
            dataset_id=dataset_id,
            document_id=file_key,
            identity=f"{payload.get('source_page') or ''}:{chunk_ord}:{payload['content_hash']}",
            ancestor_ids=[item[2] for item in heading_stack],
            depth=len(heading_stack) + 1,
            node_kind="table_row" if payload.get("type") == "table_row" else "chunk",
        )
        for key, value in hierarchy.items():
            payload.setdefault(key, value)

        source_page = payload.get("source_page") or payload.get("page") or payload.get("page_number")
        table_index = payload.get("table_index")
        if source_page is not None:
            group_key = f"page:{source_page}:table:{table_index or ''}"
            context_kind = "table_page" if payload.get("type") == "table_row" else "pdf_page"
        else:
            group_key = f"window:{chunk_ord // window_size}"
            context_kind = "markdown_window"
        grouped.setdefault(group_key, []).append(chunk_ord)
        payload.setdefault("context_kind", context_kind)

    for parent_ord, (group_key, indexes) in enumerate(grouped.items()):
        parent_id = _content_hash(f"{dataset_id}:{file_key}:{group_key}")[:24]
        heading = ""
        for idx in indexes:
            candidate = str(file_nodes[idx].get("payload", {}).get("section_heading") or "")
            if candidate:
                heading = candidate
                break
        for idx in indexes:
            payload = file_nodes[idx].setdefault("payload", {})
            payload.setdefault("parent_id", parent_id)
            payload.setdefault("parent_ord", parent_ord)
            payload.setdefault("parent_heading", heading)

    for idx, file_node in enumerate(file_nodes):
        payload = file_node.setdefault("payload", {})
        parent_id = payload.get("parent_id")
        if idx > 0 and file_nodes[idx - 1].get("payload", {}).get("parent_id") == parent_id:
            payload.setdefault("context_before", _compact_text(str(file_nodes[idx - 1].get("text") or "")))
        if idx + 1 < len(file_nodes) and file_nodes[idx + 1].get("payload", {}).get("parent_id") == parent_id:
            payload.setdefault("context_after", _compact_text(str(file_nodes[idx + 1].get("text") or "")))
    file_nodes.extend(navigation_nodes.values())


def _prepare_named_sparse_nodes(file_nodes: list[dict], file_key: str) -> list[dict]:
    """Attach sparse vectors after every evidence/navigation node is finalized."""
    from backend.inference.bm25_sparse import encode_bm25

    searchable_nodes: list[dict] = []
    for node in file_nodes:
        sparse_vec = encode_bm25(str(node["text"]))
        if not sparse_vec:
            logger.warning(
                "Skipping non-searchable node with empty sparse vector: file=%s doc_id=%s",
                file_key,
                node.get("doc_id", ""),
            )
            continue
        node["_rrf_sparse_vector"] = sparse_vec
        searchable_nodes.append(node)
    return searchable_nodes
# ── Прямой клиент эмбеддингов (httpx, без llama-index) ───────────────────────


# ── SQLite метабаза ───────────────────────────────────────────────────────────


# ── Основной адаптер ──────────────────────────────────────────────────────────

