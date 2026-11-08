"""Chat history routes for LES Proxy."""

from __future__ import annotations

import asyncio
import logging
import json
import os
from pathlib import Path
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel, field_validator

from backend.rag_config import rag_meta_db_path
from backend.workspace_native import catalog_call
from proxy.security import require_user
from proxy.services.chat_persistence_service import ensure_chat_history_schema
from proxy.services.saferag_service import SAFE_FALLBACK
from proxy.services.context_memory_service import get_chat_profile

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/api/chat", tags=["chat"])

NEGATIVE_FEEDBACK = {"bad_answer", "incorrect", "wrong_dataset", "bad_source"}


class ChatFeedbackRequest(BaseModel):
    feedback: str
    comment: Optional[str] = None
    correct_answer: Optional[str] = None
    correct_dataset_filter: Optional[str] = None

    @field_validator("feedback")
    @classmethod
    def feedback_allowed(cls, value: str) -> str:
        normalized = value.strip().lower()
        allowed = {"correct", "bad_answer", "incorrect", "partial", "wrong_dataset", "bad_source"}
        if normalized not in allowed:
            raise ValueError(f"feedback must be one of: {', '.join(sorted(allowed))}")
        return normalized


def _json_list(raw: str | None) -> list:
    try:
        value = json.loads(raw or "[]")
        return value if isinstance(value, list) else []
    except Exception:
        return []


def _feedback_log_path() -> Path:
    return Path(os.getenv("CHAT_FEEDBACK_LOG_PATH", "logs/chat_feedback.jsonl"))


def _preview(text: str | None, limit: int = 500) -> str:
    value = " ".join(str(text or "").split())
    return value[:limit]


def _append_feedback_event(event: dict) -> None:
    path = _feedback_log_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(event, ensure_ascii=False, sort_keys=True) + "\n")


@router.get("/history")
async def get_chat_history(limit: int = 40, session_id: Optional[str] = None, _user=Depends(require_user)):
    """Return recent chat messages, optionally scoped to a session."""
    try:
        await asyncio.to_thread(ensure_chat_history_schema, rag_meta_db_path())
        return await asyncio.to_thread(catalog_call, rag_meta_db_path(), 'chat_history_messages',
                                       session_id=session_id, limit=limit)
    except Exception as e:
        logger.warning("[HISTORY] %s", e)
        raise HTTPException(503, 'Не удалось прочитать историю диалога. Переписка не удалена; повторите загрузку или откройте диагностику.') from e


@router.get("/sessions")
async def get_chat_sessions(limit: int = 50, _user=Depends(require_user)):
    """Return persisted sessions through the native catalog owner."""
    try:
        return await asyncio.to_thread(catalog_call, rag_meta_db_path(), 'chat_history_sessions', limit=limit)
    except Exception as e:
        logger.warning('[SESSIONS] %s', e)
        raise HTTPException(503, 'Не удалось прочитать список диалогов. Переписка не удалена; повторите загрузку.') from e


@router.get("/memory/{session_id}")
async def get_chat_memory(session_id: str, _user=Depends(require_user)):
    """Return deterministic context profile for a chat session."""
    profile = get_chat_profile(session_id)
    if not profile:
        raise HTTPException(status_code=404, detail="chat memory profile not found")
    return profile


@router.post("/history/{history_id}/feedback")
async def save_chat_feedback(history_id: int, req: ChatFeedbackRequest, _user=Depends(require_user)):
    """Store user confirmation/correction for a chat answer."""
    try:
        feedback_user = getattr(_user, "holder", "") or getattr(_user, "source", "")
        row = await asyncio.to_thread(
            catalog_call, rag_meta_db_path(), 'chat_history_feedback',
            id=history_id, safe_fallback=SAFE_FALLBACK, feedback=req.feedback, comment=req.comment or '',
            correct_answer=req.correct_answer or '', correct_dataset_filter=req.correct_dataset_filter or '',
            feedback_user=feedback_user,
        )
        if row is None:
            raise HTTPException(status_code=404, detail="chat history row not found")
        event = {
            "event": "chat_feedback",
            "history_id": row["id"],
            "feedback": row["feedback_status"],
            "comment": row["feedback_comment"] or "",
            "correct_dataset_filter": row["feedback_correct_dataset_filter"] or "",
            "feedback_at": row["feedback_at"],
            "user": feedback_user,
            "question": _preview(row["question"], 500),
            "answer_preview": _preview(row["answer"], 700),
            "sources": [source for source in (row["sources"] or "").split(",") if source],
            "crag_status": row["crag_status"] or "",
            "requested_dataset_filter": row["requested_dataset_filter"] or "",
            "effective_dataset_filter": row["effective_dataset_filter"] or "",
            "source_dataset_names": _json_list(row["source_dataset_names"]),
            "source_dataset_mismatch": bool(row["source_dataset_mismatch"]),
            "route_channel": row["route_channel"] or "",
            "route_reason": row["route_reason"] or "",
            "retrieval_quality": row["retrieval_quality"] or "",
            "cache_type": row["cache_type"] or "",
            "validation_enabled": bool(row["validation_enabled"]),
        }
        _append_feedback_event(event)
        log_message = (
            "[CHAT_FEEDBACK] feedback=%s history_id=%s user=%s crag=%s route=%s/%s "
            "effective_filter=%s sources=%s question=%r"
        )
        log_args = (
            event["feedback"],
            event["history_id"],
            event["user"],
            event["crag_status"],
            event["route_channel"],
            event["route_reason"],
            event["effective_dataset_filter"],
            ",".join(event["sources"]),
            event["question"],
        )
        if event["feedback"] in NEGATIVE_FEEDBACK:
            logger.warning(log_message, *log_args)
        else:
            logger.info(log_message, *log_args)
        return {
            "status": "saved",
            "history_id": row["id"],
            "feedback": row["feedback_status"],
            "comment": row["feedback_comment"],
            "correct_dataset_filter": row["feedback_correct_dataset_filter"],
            "feedback_at": row["feedback_at"],
            "log": str(_feedback_log_path()),
        }
    except HTTPException:
        raise
    except Exception as e:
        logger.warning("[FEEDBACK] %s", e)
        raise HTTPException(status_code=500, detail="failed to save feedback")


@router.get("/learning")
async def get_learning_history(
    limit: int = 100,
    confirmed_only: bool = False,
    _user=Depends(require_user),
):
    """Return confirmed/successful answers from the native catalog owner."""
    try:
        await asyncio.to_thread(ensure_chat_history_schema, rag_meta_db_path())
        return await asyncio.to_thread(catalog_call, rag_meta_db_path(), 'chat_history_learning',
                                       limit=limit, confirmed_only=confirmed_only)
    except Exception as e:
        logger.warning('[LEARNING] %s', e)
        raise HTTPException(503, 'Не удалось прочитать обратную связь. Данные сохранены; повторите загрузку.') from e
