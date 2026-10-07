"""Chat history schema and transactional history writes."""
from __future__ import annotations
import logging
import sqlite3
import json
from typing import Any
from backend.rag_config import rag_meta_db_path
from proxy.services.context_memory_service import update_chat_profile
from proxy.services.saferag_service import SAFE_FALLBACK
from proxy.services import chat_request_contracts

logger = logging.getLogger(__name__)

CHAT_HISTORY_EXTRA_COLUMNS = {
    "attachment_context": "TEXT DEFAULT ''",
    "route_channel": "TEXT DEFAULT ''",
    "route_reason": "TEXT DEFAULT ''",
    "requested_dataset_filter": "TEXT DEFAULT ''",
    "effective_dataset_filter": "TEXT DEFAULT ''",
    "resolved_dataset_ids": "TEXT DEFAULT '[]'",
    "resolved_dataset_names": "TEXT DEFAULT '[]'",
    "source_dataset_ids": "TEXT DEFAULT '[]'",
    "source_dataset_names": "TEXT DEFAULT '[]'",
    "source_dataset_mismatch": "INTEGER DEFAULT 0",
    "query_route_json": "TEXT DEFAULT '{}'",
    "retrieval_trace_json": "TEXT DEFAULT '{}'",
    "artifact_json": "TEXT DEFAULT '{}'",
    "retrieval_quality": "TEXT DEFAULT ''",
    "cache_type": "TEXT DEFAULT ''",
    "validation_enabled": "INTEGER DEFAULT 1",
    "success": "INTEGER DEFAULT 0",
    "feedback_status": "TEXT DEFAULT ''",
    "feedback_comment": "TEXT DEFAULT ''",
    "feedback_correct_answer": "TEXT DEFAULT ''",
    "feedback_correct_dataset_filter": "TEXT DEFAULT ''",
    "feedback_at": "TEXT DEFAULT NULL",
    "feedback_user": "TEXT DEFAULT ''",
}


def ensure_chat_history_schema(conn: sqlite3.Connection) -> None:
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS chat_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp DATETIME DEFAULT CURRENT_TIMESTAMP,
            question TEXT,
            answer TEXT,
            sources TEXT,
            crag_status TEXT,
            latency_sec REAL,
            tokens INTEGER,
            session_id TEXT DEFAULT NULL
        )
        """
    )
    cols = [r[1] for r in conn.execute("PRAGMA table_info(chat_history)").fetchall()]
    if "session_id" not in cols:
        conn.execute("ALTER TABLE chat_history ADD COLUMN session_id TEXT DEFAULT NULL")
        cols.append("session_id")
    for name, ddl in CHAT_HISTORY_EXTRA_COLUMNS.items():
        if name not in cols:
            conn.execute(f"ALTER TABLE chat_history ADD COLUMN {name} {ddl}")
    conn.execute(
        """
        UPDATE chat_history
        SET success=1
        WHERE COALESCE(success, 0)=0
          AND crag_status IN ('VERIFIED', 'UNVALIDATED')
          AND COALESCE(answer, '') <> ''
          AND answer <> ?
        """,
        (SAFE_FALLBACK,),
    )


def _json_text(value: Any) -> str:
    try:
        return json.dumps(value, ensure_ascii=False, sort_keys=True)
    except Exception:
        return json.dumps(str(value), ensure_ascii=False)


def _history_success(crag_status: str, answer: str) -> int:
    if not answer or answer == SAFE_FALLBACK:
        return 0
    return 1 if crag_status in {"VERIFIED", "UNVALIDATED"} else 0


def save_chat_history(
    *,
    question: str,
    answer: str,
    sources: list[str],
    crag_status: str,
    latency_sec: float,
    tokens: int,
    session_id: str | None,
    requested_dataset_filter: str | None = None,
    effective_dataset_filter: str | None = None,
    resolved_dataset_ids: list[str] | None = None,
    resolved_dataset_names: list[str] | None = None,
    source_dataset_ids: list[str] | None = None,
    source_dataset_names: list[str] | None = None,
    query_route: dict[str, Any] | None = None,
    retrieval_trace: dict[str, Any] | None = None,
    artifact: dict[str, Any] | None = None,
    cache_type: str = "",
    validation_enabled: bool = True,
    success: int | None = None,
    attachment_context: str | None = None,
) -> int:
    resolved_set = set(resolved_dataset_ids or [])
    source_set = set(source_dataset_ids or [])
    source_dataset_mismatch = int(bool(resolved_set and source_set and not source_set.issubset(resolved_set)))
    route = query_route or {}
    trace = retrieval_trace or {}
    quality = ""
    if isinstance(trace.get("quality"), dict):
        quality = str(trace["quality"].get("status") or "")
    quality = quality or str(trace.get("quality_status") or "")
    success_value = _history_success(crag_status, answer) if success is None else int(bool(success))
    with sqlite3.connect(rag_meta_db_path()) as conn:
        ensure_chat_history_schema(conn)
        from proxy.services.chat_durability_service import write_history_row
        history_id = write_history_row(conn,
            ['question', 'answer', 'sources', 'crag_status', 'latency_sec', 'tokens', 'session_id', 'route_channel', 'route_reason', 'requested_dataset_filter', 'effective_dataset_filter', 'resolved_dataset_ids', 'resolved_dataset_names', 'source_dataset_ids', 'source_dataset_names', 'source_dataset_mismatch', 'query_route_json', 'retrieval_trace_json', 'artifact_json', 'retrieval_quality', 'cache_type', 'validation_enabled', 'success', 'attachment_context'],
            (
                question,
                answer,
                ",".join(sources),
                crag_status,
                latency_sec,
                tokens,
                session_id,
                str(route.get("channel") or ""),
                str(route.get("reason") or ""),
                requested_dataset_filter or "",
                effective_dataset_filter or "",
                _json_text(resolved_dataset_ids or []),
                _json_text(resolved_dataset_names or []),
                _json_text(source_dataset_ids or []),
                _json_text(source_dataset_names or []),
                source_dataset_mismatch,
                _json_text(route),
                _json_text(trace),
                _json_text(artifact or {}),
                quality,
                cache_type,
                int(bool(validation_enabled)),
                success_value,
                attachment_context or "",
            ),
        )
    try:
        update_chat_profile(
            session_id=session_id,
            question=question,
            answer=answer,
            crag_status=crag_status,
            route=route,
            requested_dataset_filter=requested_dataset_filter,
            effective_dataset_filter=effective_dataset_filter,
            resolved_dataset_ids=resolved_dataset_ids or [],
            resolved_dataset_names=resolved_dataset_names or [],
            source_dataset_ids=source_dataset_ids or [],
            source_dataset_names=source_dataset_names or [],
            success=success_value,
        )
    except Exception as err:  # профиль не должен ломать ответ/историю
        logger.warning("[CONTEXT_MEMORY] chat profile update skipped: %s", err)
    return history_id


def _persist_recovered_stream_history(
    req: chat_request_contracts.ChatRequest,
    payload: dict[str, Any],
) -> dict[str, Any]:
    """Persist an already useful recovered SSE answer for session reopen."""
    if payload.get("history_id"):
        return payload
    try:
        sources = [
            str(source.get("source_ref") or source.get("ref") or source.get("path") or source)
            if isinstance(source, dict)
            else str(source)
            for source in (payload.get("sources") or [])
        ]
        history_id = save_chat_history(
            question=req.question,
            answer=str(payload.get("answer") or ""),
            sources=sources,
            crag_status="INTERRUPTED" if payload.get("partial") else str(payload.get("crag_status") or "UNVALIDATED"),
            latency_sec=0.0,
            tokens=int(
                ((payload.get("retrieval_trace") or {}).get("stream_recovery") or {}).get(
                    "tokens"
                )
                or 0
            ),
            session_id=req.session_id,
            query_route={
                "channel": "stream_recovery",
                "operation": "recovered_partial_answer",
            },
            retrieval_trace=(
                payload.get("retrieval_trace")
                if isinstance(payload.get("retrieval_trace"), dict)
                else {}
            ),
            artifact=(
                payload.get("artifact")
                if isinstance(payload.get("artifact"), dict)
                else None
            ),
            cache_type=str(payload.get("cache") or "stream_recovered"),
            validation_enabled=False,
            success=False,
        )
        if history_id:
            payload = {**payload, "history_id": history_id}
    except Exception as error:  # persistence must not hide the recovered answer
        logger.warning("[CHAT/STREAM] recovered history save failed: %s", error)
    return payload
