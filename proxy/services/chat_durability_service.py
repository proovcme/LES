"""Commit visible stream fragments to the existing history before delivering SSE.

A request owns one history row from its first receipt through final persistence.
SQLite transactions keep recovery idempotent; no model or tool is replayed.
"""
from contextvars import ContextVar
from backend.rag_config import rag_meta_db_path
from backend.workspace_native import catalog_call
from proxy.services.saferag_service import SAFE_FALLBACK

_history_id = ContextVar('durable_chat_history_id', default=None)


def begin(request):
    from proxy.services.chat_session_service import get_session
    if not request.session_id or get_session(request.session_id) is None:
        return None
    return catalog_call(rag_meta_db_path(), 'chat_history_begin', safe_fallback=SAFE_FALLBACK,
                        session_id=request.session_id, question=request.question,
                        attachment_context=request.attachment_context or '')


def current_history_id():
    return _history_id.get()


def checkpoint(history_id, event, data):
    if history_id is None or event not in {'token', 'reset'}:
        return
    catalog_call(rag_meta_db_path(), 'chat_history_checkpoint', id=history_id, event=event, text=str(data or ''))


def interrupt(history_id=None):
    """Called for one finished/aborted request, or all orphan rows on API startup."""
    return catalog_call(rag_meta_db_path(), 'chat_history_interrupt', id=history_id)
