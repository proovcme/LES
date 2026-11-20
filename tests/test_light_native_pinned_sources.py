import json
import sqlite3
from contextlib import closing

import pytest
from openpyxl import Workbook

from tests.test_light_native_api import wait_for
from tests.test_light_native_chat_turn import native_turn


def index_folder(turn, folder, name):
    dataset = turn.client.post('/api/rag/datasets', json={'name': name}).raise_for_status().json()['id']
    job = turn.client.post('/api/rag/index-external', json={
        'path': str(folder), 'dataset_id': dataset, 'background': True,
    }).raise_for_status().json()['intake_job_id']
    wait_for(lambda: turn.client.get('/api/light/intake', params={'id': job}).json().get('status') == 'completed', timeout=60)
    with closing(sqlite3.connect(turn.database)) as db:
        path, doc_id = db.execute("SELECT file_name,id FROM documents WHERE dataset_id=? AND status='INDEXED'", (dataset,)).fetchone()
    return dataset, path, doc_id


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
