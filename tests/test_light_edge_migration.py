"""A real native Edge process imports, resumes and preserves source identity without Python services."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import uuid

import pytest


@pytest.fixture
def binary():
    value = os.environ.get('LES_TEST_EDGE_VECTOR')
    if not value:
        pytest.skip('Build les-vector and set LES_TEST_EDGE_VECTOR')
    return Path(value).resolve(strict=True)


def source(root, count=192, duplicate=False):
    path = root / 'Исходник 日本語.jsonl'
    rows = []
    for index in range(count):
        row = {'id': str(uuid.uuid5(uuid.NAMESPACE_DNS, f'les-fixture-{index if not duplicate else index % 128}')),
               'vector': {'dense': [0.6, 0.8, 0.0], 'bm25_sparse': {'indices': [17, 2087609233], 'values': [0.69314718, 1.09861228]}},
               'payload': {'dataset_id': 'test', 'file_name': 'Норматив 日本語.pdf', 'source_sha256': 'a' * 64,
                           'page': 5 + index, 'text': f'Пункт {index}. Проверяемая цитата.', 'counter': 18446744073709551615}}
        rows.append(row)
    path.write_text(''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in rows), encoding='utf-8')
    manifest = root / 'manifest.json'
    manifest.write_text(json.dumps({'schema': 'les.edge-import.v1', 'collection': 'les_test', 'source': str(path),
        'source_sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'points': count,
        'vectors': {'dense': {'size': 3, 'distance': 'Cosine'}}, 'sparse_vectors': {'bm25_sparse': {}}}), encoding='utf-8')
    return path, manifest, rows


def start(binary, *args):
    return subprocess.Popen([str(binary), *map(str, args)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True, encoding='utf-8',
                            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))


def complete(binary, *args):
    child = start(binary, *args)
    out, err = child.communicate(timeout=60)
    return child.returncode, out, err


def call(child, action, body):
    child.stdin.write(json.dumps({'id': 'test', 'action': action, 'body': body}) + '\n'); child.stdin.flush()
    response = json.loads(child.stdout.readline())
    assert response['id'] == 'test'
    assert 'error' not in response, response
    return response['result']


def test_import_keeps_float_vectors_payloads_ids_and_one_storage_owner(binary, tmp_path):
    path, manifest, rows = source(tmp_path)
    before = path.read_bytes(); target = tmp_path / 'Новое хранилище'
    code, out, err = complete(binary, 'import', manifest, target)
    assert code == 0, err
    assert json.loads(out.splitlines()[-1])['proof']['payloads_equal'] is True
    child = start(binary, 'serve-stdio', target)
    try:
        points = call(child, 'retrieve', {'ids': [rows[0]['id']], 'with_vector': True})
        assert points[0]['payload'] == rows[0]['payload']
        assert points[0]['vector']['dense'] == pytest.approx(rows[0]['vector']['dense'], abs=1e-6)
        assert points[0]['vector']['bm25_sparse']['indices'] == [17, 2087609233]
        result = call(child, 'query', {'query': [0.6, 0.8, 0.0], 'using': 'dense', 'limit': 5,
            'filter': {'must': [{'key': 'page', 'match': {'value': 5}}]}})
        assert [row['id'] for row in result['points']] == [rows[0]['id']]
        code, _, err = complete(binary, 'serve-stdio', target)
        assert code == 1 and 'EDGE_STORAGE_BUSY' in err
    finally:
        child.stdin.close(); child.wait(timeout=10)
    assert path.read_bytes() == before


def test_real_process_exit_resumes_the_durable_cursor(binary, tmp_path):
    path, manifest, rows = source(tmp_path, count=4096)
    before = path.read_bytes(); target = tmp_path / 'candidate'
    child = start(binary, 'import', manifest, target)
    while True:
        line = child.stdout.readline()
        assert line, (child.wait(timeout=5), child.stderr.read())
        event = json.loads(line)
        if event.get('phase') == 'importing' and event.get('completed', 0) >= 256:
            child.kill(); break
    child.wait(timeout=5)
    code, _, err = complete(binary, 'serve-stdio', target)
    assert code == 1 and 'EDGE_MIGRATION_NOT_READY' in err
    code, out, err = complete(binary, 'import', manifest, target)
    assert code == 0, err
    first = json.loads(out.splitlines()[0])
    assert first['resumed'] is True and first['completed'] >= 256
    child = start(binary, 'serve-stdio', target)
    try:
        assert call(child, 'count', {'exact': True})['count'] == len(rows)
        assert call(child, 'retrieve', {'ids': [rows[-1]['id']]})[0]['payload'] == rows[-1]['payload']
    finally:
        child.stdin.close(); child.wait(timeout=10)
    assert path.read_bytes() == before


def test_duplicates_and_changed_exports_never_become_ready(binary, tmp_path):
    path, manifest, _ = source(tmp_path, duplicate=True)
    target = tmp_path / 'candidate'
    code, _, err = complete(binary, 'import', manifest, target)
    assert code == 1 and 'EDGE_POINT_COUNT_MISMATCH' in err
    code, _, err = complete(binary, 'serve-stdio', target)
    assert code == 1 and 'EDGE_MIGRATION_NOT_READY' in err
    path.write_bytes(path.read_bytes() + b'{}\n')
    code, _, err = complete(binary, 'import', manifest, target)
    assert code == 1 and 'EDGE_MIGRATION_SOURCE_CHANGED' in err


def test_crash_during_bm25_publication_rebuilds_before_accepting_queries(binary, tmp_path):
    path, manifest, rows = source(tmp_path, count=8192)
    before = hashlib.sha256(path.read_bytes()).hexdigest()
    target = tmp_path / 'lexical-interrupted'
    child = start(binary, 'import', manifest, target)
    try:
        while True:
            line = child.stdout.readline()
            assert line, child.stderr.read()
            event = json.loads(line)
            if event.get('phase') == 'lexical' and event.get('completed', 0) >= 128:
                child.kill()
                break
        child.wait(timeout=5)
        code, _, err = complete(binary, 'serve-stdio', target)
        assert code == 1 and 'EDGE_MIGRATION_NOT_READY' in err
        code, out, err = complete(binary, 'import', manifest, target)
        assert code == 0, err
        assert json.loads(out.splitlines()[-1])['proof']['ids_equal'] is True
        owner = start(binary, 'serve-stdio', target)
        try:
            result = call(owner, 'hybrid_search', {'text': 'Проверяемая цитата',
                'vector': [0.6, 0.8, 0.0], 'dense_name': 'dense', 'limit': 5,
                'filters': {'dataset_ids': ['test']}})
            assert len(result['points']) == 5
            assert result['channels'] == ['dense', 'bm25_postings']
            assert all(p['payload']['dataset_id'] == 'test' for p in result['points'])
        finally:
            owner.stdin.close(); owner.wait(timeout=10)
        assert hashlib.sha256(path.read_bytes()).hexdigest() == before
    finally:
        if child.poll() is None:
            child.kill(); child.wait(timeout=5)
