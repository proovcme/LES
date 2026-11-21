import hashlib
import json
import sqlite3
from contextlib import closing

import pytest
from openpyxl import Workbook

from tests.test_light_native_api import wait_for
from tests.test_light_native_chat_turn import native_turn
from tests.pdf_fixtures import write_pdf


@pytest.mark.parametrize('native_turn', ['public'], indirect=True)
def test_selected_originals_reach_chat_without_a_search_index(native_turn, tmp_path):
    turn = native_turn
    dataset = turn.client.post('/api/rag/datasets', json={'name': 'Drawings'}).raise_for_status().json()['id']
    originals = {}
    for key, path, fact in [('a', 'A.pdf', 'ALPHA 100'), ('b', 'B.pdf', 'BETA 200'),
                            ('c', 'current/Notes.pdf', 'GAMMA only after inspection'),
                            ('x', 'archive/Notes.pdf', 'OBSOLETE 999')]:
        file = write_pdf(tmp_path / f'{key}.pdf', [fact])
        digest = hashlib.sha256(file.read_bytes()).hexdigest()
        with closing(sqlite3.connect(turn.database)) as db, db:
            db.execute('INSERT INTO documents (id,dataset_id,file_name,file_hash,source_path,status) VALUES (?,?,?,?,?,?)',
                       (key, dataset, path, digest, str(file), 'INDEXED'))
        originals[key] = (path, digest, file, fact)
    request = {'question': 'Compare the three specified records and their qualification.', 'mode': 'search',
               'retrieval_mode': 'hybrid', 'scope': {'scope_type': 'dataset', 'dataset_ids': [dataset]},
               'target_files': ['A.pdf', 'B.pdf', 'current/Notes.pdf']}
    pinned_scope = {**request['scope'], 'document_ids': ['a', 'b', 'c']}
    session = turn.client.post('/api/workspace/sessions', json={}).raise_for_status().json()['session_id']
    for targets in ({'target_files': request['target_files']},
                    {'target_files': [], 'scope': pinned_scope},
                    {'target_files': [], 'scope': None, 'session_id': session}):
        if targets.get('session_id'):
            saved = turn.client.patch(f'/api/workspace/sessions/{session}', json={
                'scope': {**pinned_scope, 'project_ids': [], 'selected_sources_only': False}}).raise_for_status().json()
            assert saved['scope']['document_ids'] == ['a', 'b', 'c']
        turn.scripts.append({'text': 'ALPHA 100 [1]. BETA 200 [2]. GAMMA only after inspection [3].'})
        before = len(turn.seen)
        response = turn.client.post('/api/chat', json={**request, **targets})
        assert response.status_code == 200, response.text
        assert len(turn.seen) == before + 1
        messages = json.dumps(turn.seen[-1]['messages'], ensure_ascii=False)
        assert all(originals[key][3] in messages for key in 'abc')
        assert 'OBSOLETE 999' not in messages
        sources = response.json()['source_map']
        assert [(s['doc_id'], s['source_sha256'], s['source_page']) for s in sources] == [
            (key, originals[key][1], 1) for key in 'abc']
        for source in sources:
            raw = turn.client.get(source['url'].split('#')[0]).raise_for_status()
            assert hashlib.sha256(raw.content).hexdigest() == source['source_sha256']
    before = len(turn.seen)
    outside = turn.client.post('/api/rag/datasets', json={'name': 'Another folder'}).raise_for_status().json()['id']
    denied = turn.client.post('/api/chat', json={**request, 'target_files': [], 'scope': {
        **pinned_scope, 'dataset_ids': [outside]}})
    assert denied.json()['code'] == 'SOURCE_DOCUMENT_NOT_FOUND'
    for path, code in [('Notes.pdf', 'SOURCE_IDENTITY_AMBIGUOUS'), ('missing.pdf', 'SOURCE_DOCUMENT_NOT_FOUND')]:
        response = turn.client.post('/api/chat', json={**request, 'target_files': ['A.pdf', path]})
        assert response.json()['code'] == code and path in response.json()['detail']
    write_pdf(originals['c'][2], ['Replaced original'])
    response = turn.client.post('/api/chat', json=request)
    assert response.json()['code'] == 'PDF_REVISION_CHANGED'
    with closing(sqlite3.connect(turn.database)) as db, db:
        db.execute("DELETE FROM documents WHERE id='c'")
    removed = turn.client.post('/api/chat', json=request)
    assert removed.json()['code'] == 'SOURCE_DOCUMENT_NOT_FOUND'
    assert 'current/Notes.pdf' in removed.json()['detail']
    assert len(turn.seen) == before


def index_folder(turn, folder, name):
    dataset = turn.client.post('/api/rag/datasets', json={'name': name}).raise_for_status().json()['id']
    job = turn.client.post('/api/rag/index-external', json={
        'path': str(folder), 'dataset_id': dataset, 'background': True,
    }).raise_for_status().json()['intake_job_id']
    wait_for(lambda: turn.client.get('/api/light/intake', params={'id': job}).json().get('status') == 'completed', timeout=60)
    with closing(sqlite3.connect(turn.database)) as db:
        path, doc_id = db.execute("SELECT file_name,id FROM documents WHERE dataset_id=? AND status='INDEXED'", (dataset,)).fetchone()
    return dataset, path, doc_id


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_folder_search_keeps_full_pool_and_sends_profile_allowance(native_turn, tmp_path):
    folder = tmp_path / 'manuals'
    folder.mkdir()
    markers = [f'CALIBRATION_{i:03d}' for i in range(18)]
    for i, marker in enumerate(markers):
        (folder/f'{i}.txt').write_text(f'Inspection calibration record. Designation {marker}.', encoding='utf-8')
    dataset, _, _ = index_folder(native_turn, folder, 'Inspection')
    native_turn.scripts.append({'text':'Inspection recorded [Источник 1].'})
    response = native_turn.client.post('/api/chat', json={
        'question':'What is recorded about calibration?', 'mode':'search', 'retrieval_mode':'hybrid',
        'scope':{'scope_type':'datasets', 'dataset_ids':[dataset]},
    }).raise_for_status().json()
    wire = json.dumps(native_turn.seen[0]['messages'], ensure_ascii=False)
    trace = response['retrieval_trace']
    assert len(trace['prefetch']['candidate_documents']) == len(markers)
    assert sum(marker in wire for marker in markers) == 6
    assert len(trace['contexts']) == 1 and len(trace['contexts'][0]['sources']) == 6
    assert any(o['reason'] == 'model_evidence_limit' for o in trace['contexts'][0]['context']['omissions'])


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_search_id_pins_dataset_and_path(native_turn, tmp_path):
    docs = []
    for dataset, value in [('A', '143'), ('B', '987')]:
        folder = tmp_path / dataset / 'register'
        folder.mkdir(parents=True)
        (folder / 'common.txt').write_text(f'Equipment registry control pressure equals {value} kPa.', encoding='utf-8')
        docs.append(index_folder(native_turn, folder, dataset))
    (a, path_a, doc_a), (b, path_b, _) = docs
    assert path_a == path_b
    args = {'q': '987 kPa', 'doc_id': doc_a, 'dataset_ids': [a, b]}

    def search(**overrides):
        return native_turn.client.post('/api/rag/sources/call', json={'tool': 'search_sources', 'args': {**args, **overrides}})

    result = search().raise_for_status().json()
    assert {source['doc_id'] for source in result['sources']} == {doc_a}
    assert {doc['doc_id'] for doc in result['result']['candidate_documents']} == {doc_a}
    catalog = search(q='common', view='documents').raise_for_status().json()['result']['documents']
    assert [doc['id'] for doc in catalog] == [doc_a]
    assert search(dataset_ids=[b]).status_code == 403


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_excel_false_zero_and_pinned_continuation(native_turn, tmp_path):
    folder = tmp_path / 'tables'
    folder.mkdir()
    workbook = Workbook()
    sheet = workbook.active
    sheet.append(['Code', 'Quantity', 'Enabled', 'Optional'])
    for code in ['PUMP', None, 'FAN']:
        sheet.append([code, 0, False, None])
    workbook.save(folder / 'controls.xlsx')
    _, _, doc = index_folder(native_turn, folder, 'Tables')

    def read(args):
        return native_turn.client.post('/api/rag/sources/call', json={'tool': 'read_excel_source', 'args': args})

    args = {'doc_id': doc, 'sheet_name': sheet.title, 'row_start': 2, 'row_end': 4, 'limit': 1}
    for row in range(2, 5):
        result = read(args).raise_for_status().json()['result']
        passage, = result['passages']
        assert passage['source']['locator']['row'] == row
        assert 'Quantity: 0' in passage['text'] and 'Enabled: False' in passage['text']
        assert 'Optional:' not in passage['text']
        if row == 3:
            assert 'Code:' not in passage['text']
        if row < 4:
            args = result['continuation']
            stale = read({**args, 'view_sha256': '0' * 64})
            assert stale.json()['code'] == 'PARQUET_SOURCE_CHANGED'
    assert result['next_offset'] is None


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_hybrid_delivers_named_files_and_accepts_an_interval(native_turn, tmp_path):
    folder = tmp_path / 'records'
    folder.mkdir()
    records = {
        'A-1.txt': 'Working pressure recorded as ALPHA_VALUE.',
        'B-2.txt': 'Working pressure recorded as BETA_VALUE.',
        'Notes.txt': 'Qualification for the working pressure is GAMMA_NOTE.',
    }
    for name, body in records.items():
        (folder / name).write_text(body, encoding='utf-8')
    dataset, _, _ = index_folder(native_turn, folder, 'Registers')
    native_turn.scripts.append({'text': 'Range [0, 10], samples [1, 2, 3]. Records [Источники 1, 2]. Note [3].'})
    response = native_turn.client.post('/api/chat', json={
        'question': 'Report the pressure from A-1 and B-2 and the qualification from Notes.txt.',
        'mode': 'search', 'retrieval_mode': 'hybrid',
        'scope': {'scope_type': 'dataset', 'dataset_ids': [dataset]},
    })
    assert len(native_turn.seen) >= 1, response.text
    messages = json.dumps(native_turn.seen[0]['messages'], ensure_ascii=False)
    for fact in ('ALPHA_VALUE', 'BETA_VALUE', 'GAMMA_NOTE'):
        assert fact in messages
    assert response.status_code == 200, response.text
    assert response.json()['answer'] == 'Range [0, 10], samples [1, 2, 3]. Records [Источник 1] [Источник 2]. Note [Источник 3].'
    assert len(native_turn.seen) == 1

    native_turn.scripts.append({'text': 'ALPHA_VALUE [1], BETA_VALUE [2], GAMMA_NOTE [3].'})
    pinned = native_turn.client.post('/api/chat', json={
        'question': 'Compare the two pressures and state their qualification.',
        'mode': 'search', 'retrieval_mode': 'hybrid', 'target_files': list(records),
        'scope': {'scope_type': 'dataset', 'dataset_ids': [dataset]},
    })
    assert pinned.status_code == 200, pinned.text
    assert len(native_turn.seen) == 2
    messages = json.dumps(native_turn.seen[-1]['messages'], ensure_ascii=False)
    assert all(body in messages for body in records.values())
    with closing(sqlite3.connect(native_turn.database)) as db:
        originals = dict(db.execute('SELECT id,file_hash FROM documents WHERE dataset_id=?', (dataset,)))
    assert len(originals) == 3
    assert {s['doc_id']: s['source_sha256'] for s in pinned.json()['source_map']} == originals
    assert pinned.json()['retrieval_trace']['prefetch']['selection_policy'] == 'required_documents'
