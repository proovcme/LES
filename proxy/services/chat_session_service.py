"""Compatibility facade. Session storage and decisions are implemented in Rust."""
from backend import workspace_native
from backend.rag_config import rag_meta_db_path


class SessionConflictError(ValueError):
    """An existing session or draft revision cannot be reassigned."""


def get_session(session_id: str) -> dict | None:
    return workspace_native.call(rag_meta_db_path(), 'get', session_id=session_id)


def get_session_project_id(session_id: str) -> int | None:
    session = get_session(session_id)
    return session['project_id'] if session else None


def list_sessions(project_id: int | None = None) -> list[dict]:
    return workspace_native.call(rag_meta_db_path(), 'list', project_id=project_id)


def create_session(project_id=None, title='', session_id=None) -> dict:
    return workspace_native.call(rag_meta_db_path(), 'create', project_id=project_id,
                                 title=title, session_id=str(session_id) if session_id is not None else None)


def update_session(session_id, title=None, scope=None, role=None) -> dict:
    return workspace_native.call(rag_meta_db_path(), 'update', session_id=session_id,
                                 title=title, scope=scope, role=role)
