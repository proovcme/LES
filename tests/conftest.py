"""Offline and reproducible defaults for the LES unit/integration test process."""

from __future__ import annotations

import os
import gc
import shutil
import stat
import warnings
from pathlib import Path
import pytest


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item, call):
    result = yield
    report = result.get_result()
    setattr(item, '_les_report_' + call.when, report)
    if call.when == 'teardown' and not report.failed:
        _clean_completed_scratch(item)


@pytest.fixture(autouse=True)
def _local_resource_guard(request):
    if os.getenv('LES_LOCAL_RESOURCE_GUARD') == '1':
        if shutil.disk_usage(request.config.rootpath).free < 20 * 1024 ** 3:
            pytest.exit('Less than 20 GiB free: local checks stopped before filling the disk.', returncode=2)
def _clean_completed_scratch(item):
    """After every fixture closes, remove only a successful test's owned tree."""
    path = item.funcargs.get('tmp_path')
    report = getattr(item, '_les_report_call', None)
    if path is None or report is None or report.failed:
        return  # Keep the failed case for inspection; reuse basetemp on the next run.
    base = item.config._tmp_path_factory.getbasetemp().resolve()
    if path.is_symlink() or path.resolve().parent != base or not path.is_dir():
        return
    def readonly(operation, target, error):
        target = Path(target)
        if not target.resolve().is_relative_to(path.resolve()):
            raise error
        if isinstance(error, PermissionError):
            target.chmod(target.stat().st_mode | stat.S_IWRITE)
            operation(target)
        else:
            raise error
    try:
        shutil.rmtree(path, onexc=readonly)
        path.mkdir()  # Keep pytest's name reservation; cached paths must not be reused.
    except PermissionError:
        # sqlite3 connections in fixture/test reference cycles close on collection.
        gc.collect()
        try:
            shutil.rmtree(path, onexc=readonly)
            path.mkdir()
        except OSError as error:
            warnings.warn(f'Test scratch cleanup deferred: {type(error).__name__}', RuntimeWarning)
    except OSError as error:
        warnings.warn(f'Test scratch cleanup deferred: {type(error).__name__}', RuntimeWarning)


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
