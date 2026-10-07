"""Commit visible stream fragments to the existing history before delivering SSE.

A request owns one history row from its first receipt through final persistence.
SQLite transactions keep recovery idempotent; no model or tool is replayed.
"""
from contextvars import ContextVar
import sqlite3

from backend.rag_config import rag_meta_db_path

_history_id = ContextVar('durable_chat_history_id', default=None)


def begin(request):
    from proxy.services.chat_session_service import get_session
    if not request.session_id or get_session(request.session_id) is None:
        return None
    from proxy.services.chat_persistence_service import ensure_chat_history_schema
    with sqlite3.connect(rag_meta_db_path(), timeout=10) as conn:
        ensure_chat_history_schema(conn)
        cursor = conn.execute(
            "INSERT INTO chat_history (session_id,question,answer,crag_status,success,validation_enabled,attachment_context) "
            "VALUES (?,?,?,'IN_PROGRESS',0,0,?)",
            (request.session_id, request.question, '', request.attachment_context or ''))
        return cursor.lastrowid


def write_history_row(conn, columns, values):
    history_id = _history_id.get()
    if history_id is None:
        marks = ','.join('?' for _ in columns)
        return conn.execute(f"INSERT INTO chat_history ({','.join(columns)}) VALUES ({marks})", values).lastrowid
    session_id = values[columns.index('session_id')]
    changed = conn.execute(
        f"UPDATE chat_history SET {','.join(name + '=?' for name in columns)} WHERE id=? AND session_id=?",
        (*values, history_id, session_id)).rowcount
    if changed != 1:
        raise RuntimeError('Durable chat history row is missing')
    return history_id


def checkpoint(history_id, event, data):
    if history_id is None or event not in {'token', 'reset'}:
        return
    with sqlite3.connect(rag_meta_db_path(), timeout=10) as conn:
        if event == 'reset':
            conn.execute("UPDATE chat_history SET answer='',tokens=0 WHERE id=? AND crag_status='IN_PROGRESS'", (history_id,))
        else:
            conn.execute("UPDATE chat_history SET answer=COALESCE(answer,'')||?,tokens=COALESCE(tokens,0)+1 "
                         "WHERE id=? AND crag_status='IN_PROGRESS'", (str(data or ''), history_id))


def interrupt(history_id=None):
    """Called for one finished/aborted request, or all orphan rows on API startup."""
    with sqlite3.connect(rag_meta_db_path(), timeout=10) as conn:
        if not conn.execute("SELECT 1 FROM sqlite_master WHERE name='chat_history'").fetchone():
            return 0
        condition, params = (' AND id=?', (history_id,)) if history_id is not None else ('', ())
        return conn.execute("UPDATE chat_history SET crag_status='INTERRUPTED',success=0,validation_enabled=0 "
                            "WHERE crag_status='IN_PROGRESS'" + condition, params).rowcount
