"""Native registry durability, concurrent CAS and compatibility with existing rows."""
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timedelta, timezone
from pathlib import Path
import sqlite3
import subprocess
import sys
import threading

import pytest

from backend import native_core
from proxy.services.model_connection_contracts import (
    CapabilitySnapshot, ConnectionLocality, ConnectionRole,
)
from proxy.services.model_connection_registry_service import (
    ModelConnectionRegistry, ModelConnectionRegistryError, RevisionConflictError,
)


def create(registry, **changes):
    values = dict(display_name='Модель 日本語 🌲', base_url='http://127.0.0.1:11434/v1',
                  model_id='fixture:9b', locality=ConnectionLocality.LOOPBACK,
                  requested_context_tokens=24576, secret_ref=None, extension_type='ollama', actor='admin:test')
    return registry.create_connection(**(values | changes))


def test_competing_edits_and_assignments_have_one_winner(tmp_path):
    path = tmp_path/'Реестр 日本語 с пробелами.db'
    registry = ModelConnectionRegistry(path)
    first = create(registry)
    barrier = threading.Barrier(8)

    def revise(index):
        barrier.wait(timeout=10)
        try:
            return registry.revise_connection(first.connection_id, expected_revision_id=first.revision_id,
                                              actor=f'admin:{index}', model_id=f'fixture:{index}')
        except RevisionConflictError as error:
            return str(error)

    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(revise, range(8)))
    assert results.count('CONNECTION_REVISION_CONFLICT') == 7
    revisions = registry.list_revisions(first.connection_id)
    assert len(revisions) == 2 and registry.get_connection(first.connection_id) == revisions[-1]

    def bind(index):
        barrier.wait(timeout=10)
        try:
            return registry.bind_role(ConnectionRole.ANSWER, first.revision_id,
                                      expected_binding_revision=None, actor=f'admin:{index}')
        except RevisionConflictError as error:
            return str(error)

    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(bind, range(8)))
    assert results.count('ROLE_BINDING_CONFLICT') == 7
    with sqlite3.connect(path) as db:
        assert db.execute('SELECT COUNT(*) FROM les_model_connection_audit').fetchone()[0] == 3
        assert db.execute('PRAGMA foreign_key_check').fetchall() == []


def test_audit_failure_rolls_back_revision_and_head(tmp_path):
    path = tmp_path/'registry.db'
    registry = ModelConnectionRegistry(path)
    first = create(registry)
    with sqlite3.connect(path) as db:
        db.execute("""CREATE TRIGGER reject_audit BEFORE INSERT ON les_model_connection_audit
                      BEGIN SELECT RAISE(ABORT, 'fixture audit failure'); END""")
    with pytest.raises(sqlite3.OperationalError):
        registry.revise_connection(first.connection_id, expected_revision_id=first.revision_id,
                                   actor='admin:test', model_id='must-not-persist')
    assert registry.get_connection(first.connection_id) == first
    assert registry.list_revisions(first.connection_id) == (first,)


def test_reopen_keeps_pinned_roles_history_and_snapshots(tmp_path):
    path = tmp_path/'registry.db'
    registry = ModelConnectionRegistry(path)
    first = create(registry)
    binding = registry.bind_role(ConnectionRole.ANSWER, first.revision_id,
                                 expected_binding_revision=None, actor='admin:test')
    observed = datetime(2026, 10, 1, tzinfo=timezone.utc)
    snapshot = CapabilitySnapshot('cap:immutable', first.revision_id, (), observed,
                                  observed + timedelta(days=1), {'chat_protocol': 'openai_chat_v1'})
    registry.save_capability_snapshot(snapshot, actor='admin:test')
    disabled = registry.disable_connection(first.connection_id, expected_revision_id=first.revision_id, actor='admin:test')
    reopened = ModelConnectionRegistry(path)
    assert reopened.list_revisions(first.connection_id) == (first, disabled)
    assert reopened.get_role_binding(ConnectionRole.ANSWER) == binding
    assert reopened.execution_snapshot(role='answer') == (first, False, snapshot)
    with pytest.raises(ModelConnectionRegistryError, match='CAPABILITY_SNAPSHOT_EXISTS'):
        reopened.save_capability_snapshot(snapshot, actor='admin:duplicate')


def test_native_contract_validates_before_mutating(tmp_path):
    path = tmp_path/'registry.db'
    registry = ModelConnectionRegistry(path)
    first = create(registry)
    value = native_core.required('catalog', request={
        'schema': 'les.catalog.v1', 'database': str(path), 'action': 'model_registry_revise',
        'arguments': {'connection_id': first.connection_id, 'expected_revision_id': first.revision_id,
                      'actor': 'admin:test', 'changes': {'locality': 'arbitrary-network'}},
    })
    assert value['status'] == 'failed' and value['code'] == 'WORKSPACE_INVALID'
    assert registry.list_revisions(first.connection_id) == (first,)


def test_process_kill_never_leaves_an_orphan_revision_or_missing_audit(tmp_path):
    path = tmp_path/'registry.db'
    registry = ModelConnectionRegistry(path)
    first = create(registry)
    script = '''import sys
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
registry=ModelConnectionRegistry(sys.argv[1]); current=registry.get_connection(sys.argv[2])
for index in range(10000):
    current=registry.revise_connection(current.connection_id,expected_revision_id=current.revision_id,
                                      actor='admin:child',model_id=f'fixture:{index}')
    print(current.revision_no,flush=True)
'''
    child = subprocess.Popen([sys.executable, '-B', '-c', script, str(path), first.connection_id],
                             cwd=Path(__file__).resolve().parents[1], stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    try:
        # Wait on a bounded future: a failed child cannot hang the whole test run.
        pool = ThreadPoolExecutor(max_workers=1)
        try:
            line = pool.submit(child.stdout.readline).result(timeout=15)
        finally:
            if child.poll() is None:
                child.kill()
            pool.shutdown(wait=True)
        assert int(line) >= 2
        child.wait(timeout=10)
    finally:
        if child.poll() is None:
            child.kill()
        child.communicate(timeout=10)
    reopened = ModelConnectionRegistry(path)
    revisions = reopened.list_revisions(first.connection_id)
    assert reopened.get_connection(first.connection_id) == revisions[-1]
    assert [row.revision_no for row in revisions] == list(range(1, len(revisions)+1))
    with sqlite3.connect(path) as db:
        assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        assert db.execute('PRAGMA foreign_key_check').fetchall() == []
        assert db.execute('SELECT COUNT(*) FROM les_model_connection_audit').fetchone()[0] == len(revisions)
        assert db.execute("SELECT COUNT(*) FROM les_model_connection_audit WHERE revision_id=?", (revisions[-1].revision_id,)).fetchone()[0] == 1
