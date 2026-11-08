"""Offline and reproducible defaults for the LES unit/integration test process."""

from __future__ import annotations

import os
from pathlib import Path
import pytest


def pytest_configure(config):
    # pytest creates basetemp itself, but not a missing parent in a fresh clone.
    if config.option.basetemp:
        Path(config.option.basetemp).resolve().parent.mkdir(parents=True, exist_ok=True)


# Test collection/imports and health/config reads must not download models or
# depend on whether the workstation currently has internet access.
os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("HF_HUB_CACHE", str(Path(__file__).resolve().parent / '.empty-hf-cache'))
os.environ.setdefault("TRANSFORMERS_OFFLINE", "1")
os.environ.setdefault("RAG_TOKENIZER_LOCAL_FILES_ONLY", "true")
os.environ.setdefault("LES_FORMAT_WORKER_MODE", "inline")  # Unit mocks stay local; worker gates explicitly select native.


@pytest.fixture(params=['inline', 'native'])
def workspace_engine(request, monkeypatch):
    if request.param == 'native':
        if not os.getenv('LES_CORE_LIBRARY'):
            pytest.skip('Build the native core and set LES_CORE_LIBRARY for workspace parity')
        from backend import native_core
        monkeypatch.setenv('LES_CORE_MODE', 'native')
        assert native_core.supports('workspace')
    else:
        from proxy.services import chat_session_service as sessions, chat_draft_service as drafts
        from tests.oracles import chat_session_python as previous_sessions, chat_draft_python as previous_drafts
        monkeypatch.setattr(previous_sessions, 'rag_meta_db_path', lambda: sessions.rag_meta_db_path())
        monkeypatch.setattr(previous_sessions, 'SessionConflictError', sessions.SessionConflictError)
        for name in ('get_session', 'get_session_project_id', 'list_sessions', 'create_session', 'update_session'):
            monkeypatch.setattr(sessions, name, getattr(previous_sessions, name))
        for name in ('get_draft', 'save_draft'):
            monkeypatch.setattr(drafts, name, getattr(previous_drafts, name))
    return request.param


@pytest.fixture(params=['inline', 'native'])
def catalog_engine(request, monkeypatch):
    if request.param == 'native':
        if not os.getenv('LES_CORE_LIBRARY'):
            pytest.skip('Build the native core for project/memory parity')
        from backend import native_core
        monkeypatch.setenv('LES_CORE_MODE', 'native')
        assert native_core.supports('catalog')
    else:
        from proxy.services import project_service, memory_service, conversation_context_service
        from tests.oracles import project_python, memory_python, context_python
        pairs = [
            (project_service, project_python, ('create_project', 'list_projects', 'get_project',
                'set_project_status', 'delete_project', 'link_entity', 'unlink_entity', 'list_links',
                'project_for_dataset', '_datasets_in_scope', 'build_registry')),
            (memory_service, memory_python, ('create_note', 'list_notes', 'project_note_items',
                'update_note', 'delete_note', 'recall_context', 'session_memory_items',
                'session_user_questions', 'session_recent_retrieval_traces')),
            (conversation_context_service, context_python, ('get_context', 'update_context',
                'history_turns', 'context_status', 'forget_context')),
        ]
        for current, previous, names in pairs:
            monkeypatch.setattr(previous, 'rag_meta_db_path', lambda current=current: current.rag_meta_db_path())
            for name in names:
                monkeypatch.setattr(current, name, getattr(previous, name))
        monkeypatch.setattr(context_python, 'ContextConflict', conversation_context_service.ContextConflict)
    return request.param
