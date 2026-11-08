"""Cold Edge recovery admits search only after the Rust publisher finishes."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
from uuid import uuid4

import httpx
import pytest

from backend.document_catalog import MetaDB
from proxy.services.lexical_index_service import LexicalIndex
from test_light_edge_migration import source, complete, start, call
from test_light_native_api import free_port, wait_for


@pytest.mark.parametrize('phase', ['staging', 'committing', 'sparse_refresh', 'missing_document', 'missing_point'])
def test_cold_recovery_without_python_backend(tmp_path, phase):
    vector = os.getenv('LES_TEST_EDGE_VECTOR')
    api = os.getenv('LES_TEST_EDGE_API')
    if not vector or not api:
        pytest.skip('Build native Edge API/vector and set LES_TEST_EDGE_API/LES_TEST_EDGE_VECTOR')
    _, manifest, rows = source(tmp_path, count=8)
    target = tmp_path / 'Edge 日本語'
    code, _, err = complete(Path(vector), 'import', manifest, target)
    assert code == 0, err
    name = rows[0]['payload']['file_name']
    replacement = {**rows[0], 'id': str(uuid4()), 'payload': {**rows[0]['payload'],
        'doc_id': 'doc', 'text': 'Новая редакция кабелепровода', 'content_hash': 'b' * 40}}
    journal = target / 'content/.index-replacements' / hashlib.sha256(b'les_test').hexdigest()[:24] / 'journal.sqlite'
    pending = {'phase': 'committing' if phase in {'committing', 'missing_point'} else 'staging',
        'revision': 'crash-generation', 'dataset': 'test', 'file_name': name,
        'db_file_name': name, 'document_id': 'doc', 'ids': [replacement['id']]}
    if phase == 'sparse_refresh':
        pending = {'phase': 'sparse_refresh', 'revision': 'crash-generation'}
    with sqlite3.connect(journal) as conn:
        conn.execute('UPDATE replacement SET body=? WHERE id=1', [json.dumps(pending)])
    # Kill the actual owner after the durable vector update, before journal completion.
    child = start(Path(vector), 'serve-stdio', target)
    try:
        if phase not in {'sparse_refresh', 'missing_point'}:
            call(child, 'upsert', {'points': [replacement]})
        child.kill(); child.wait(timeout=5)
    finally:
        if child.poll() is None:
            child.kill(); child.wait(timeout=5)
    state = tmp_path / 'State'; state.mkdir()
    database = state / 'meta.db'
    MetaDB(str(database))
    with closing(LexicalIndex(str(database)).connect()):
        pass
    with sqlite3.connect(database) as conn:
        conn.execute("INSERT INTO datasets(id,name) VALUES('test','Test')")
        if phase != 'missing_document':
            conn.execute("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count) VALUES('doc','test',?,'PARSING',8)", [name])
    policy = state / 'auth.json'
    policy.write_text(json.dumps({'schema': 'les.native-auth-policy.v1', 'compatible': True,
        'trusted_role': 'admin', 'cors_origins': [], 'cors_loopback': True,
        'proxy_header': 'x-les-trusted-network'}), encoding='utf-8')
    port, dead_backend = free_port(), free_port()
    while port == dead_backend:
        dead_backend = free_port()
    log_path = state / 'api.log'
    with log_path.open('wb') as log, httpx.Client(base_url=f'http://127.0.0.1:{port}',
            headers={'api-key': 'edge-test'}, trust_env=False, timeout=10) as http:
        owner = subprocess.Popen([api, '--listen', f'127.0.0.1:{port}', '--upstream',
            f'http://127.0.0.1:{dead_backend}', '--database', str(database), '--state', str(state),
            '--edge-storage', str(target), '--auth-policy-file', str(policy)],
            env={**os.environ, 'LES_INTERNAL_INGEST_TOKEN': 'a' * 64,
                 'LES_LIGHT_QDRANT_API_KEY': 'edge-test'}, stdout=log, stderr=subprocess.STDOUT,
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        try:
            def finished():
                assert owner.poll() is None, log_path.read_text(encoding='utf-8', errors='replace')
                response = http.get('/collections')
                return response.status_code == 200 or response.status_code == 503 and response.json()['detail'] != 'INDEX_RECOVERING'
            wait_for(finished)
            response = http.get('/collections')
            if phase in {'missing_document', 'missing_point'}:
                expected = 'INDEX_REGISTRATION_CHANGED' if phase == 'missing_document' else 'INDEX_RECOVERY_INCOMPLETE'
                assert response.status_code == 503 and response.json()['detail'] == expected
                assert http.get('/api/profiles').status_code == 200
                assert expected in log_path.read_text(encoding='utf-8', errors='replace')
                with sqlite3.connect(journal) as conn:
                    assert json.loads(conn.execute('SELECT body FROM replacement').fetchone()[0]) == pending
                return
            response.raise_for_status()
            found = http.post('/api/light/internal-index-store', headers={'x-les-index-token': 'a' * 64},
                json={'action': 'hybrid_search', 'collection': 'les_test', 'text': 'кабелепровод',
                      'vector': [0.6, 0.8, 0.0], 'dense_name': 'dense', 'limit': 10}).raise_for_status().json()
            expected_ids = {replacement['id']} if phase == 'committing' else {row['id'] for row in rows}
            assert {p['id'] for p in found['points']} == expected_ids
            with sqlite3.connect(journal) as conn:
                assert json.loads(conn.execute('SELECT body FROM replacement').fetchone()[0])['phase'] == 'idle'
            if phase != 'sparse_refresh':
                with sqlite3.connect(database) as conn:
                    status, count, cause = conn.execute('SELECT status,chunk_count,error_code FROM documents').fetchone()
                    assert (status, count, cause) == ('PENDING', 0, 'INDEX_INTERRUPTED')
            assert 'index_startup_recovery_completed' in log_path.read_text(encoding='utf-8', errors='replace')
            # The wire compatibility route must not bypass generation checks during a later update.
            with sqlite3.connect(journal) as conn:
                conn.execute('UPDATE replacement SET body=? WHERE id=1', [json.dumps({
                    'phase': 'sparse_dirty', 'revision': 'next-generation'})])
            rejected = http.post('/collections/les_test/points/query', json={
                'query': [0.6, 0.8, 0.0], 'using': 'dense', 'limit': 5})
            assert rejected.status_code == 409 and rejected.json()['detail'] == 'INDEX_RECOVERY_REQUIRED'
            assert http.get('/api/profiles').status_code == 200
            http.post('/api/light/internal-index-store', headers={'x-les-index-token': 'a' * 64},
                json={'action': 'recover_file', 'collection': 'les_test'}).raise_for_status()
            assert http.post('/collections/les_test/points/query', json={
                'query': [0.6, 0.8, 0.0], 'using': 'dense', 'limit': 5}).status_code == 200
        finally:
            owner.terminate(); owner.wait(timeout=10)
