"""Compatibility facade. Draft compare-and-swap and transactions belong to Rust."""
from backend import workspace_native
from proxy.services import chat_session_service as sessions


def get_draft(session_id):
    return workspace_native.call(sessions.rag_meta_db_path(), 'draft_get', session_id=session_id)


def save_draft(session_id, revision, text, attachment=None):
    return workspace_native.call(sessions.rag_meta_db_path(), 'draft_save', session_id=session_id,
                                 revision=revision, text=text, attachment=attachment)
