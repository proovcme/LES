"""Evidence operations and presentation share the public native turn path."""
import hashlib
import json
import re
import sqlite3

import pytest

from tests.pdf_fixtures import write_pdf
from tests.test_light_native_api import wait_for
from tests.test_light_native_chat_turn import native_turn, source_pdf, PATH, HEADERS


def index(t, folder):
    dataset = t.client.post('/api/rag/datasets', json={'name': 'Mixed evidence'}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external', json={'path': str(folder), 'dataset_id': dataset, 'background': True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda: t.client.get('/api/light/intake', params={'id': job}).json().get('status') == 'completed', timeout=40)
    return dataset


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_mixed_discovery_exposes_physical_reader_and_preserves_original_page(native_turn, tmp_path):
    t = native_turn
    folder = tmp_path / 'sources'; folder.mkdir()
    pdf = write_pdf(folder / 'source.pdf', ['MIXED_ANCHOR Physical original proof.'])
    original = pdf.read_bytes()
    (folder / 'source.md').write_text('MIXED_ANCHOR indexed companion document.', encoding='utf-8')
    dataset = index(t, folder)
    with sqlite3.connect(t.database) as db:
        doc = db.execute("SELECT id FROM documents WHERE dataset_id=? AND file_name LIKE '%.pdf'", [dataset]).fetchone()[0]
    def read(body):
        names = {tool['function']['name'] for tool in body['tools']}
        assert {'search_sources', 'read_source', 'read_pdf_source'} <= names
        assert 'reader_contract' in json.dumps(body['messages'])
        assert 'Финальный ответ по источникам: верни только JSON' not in json.dumps(body['messages'], ensure_ascii=False)
        return {'tool': 'read_pdf_source', 'args': {'doc_id': doc, 'page': 1}}
    def answer(body):
        text = json.dumps(body['messages'], ensure_ascii=False)
        indexes = re.findall(r'\[(?:Источник|Source) (\d+)\]', text)
        assert indexes, text
        return {'text': f'Physical original proof [Источник {max(map(int, indexes))}]'}
    t.scripts.extend([read, answer])
    response = t.client.post('/api/chat', json={'question': 'MIXED_ANCHOR: verify the physical original PDF.', 'mode': 'search', 'retrieval_mode': 'hybrid',
        'scope': {'scope_type': 'dataset', 'dataset_ids': [dataset], 'selected_sources_only': True}})
    assert response.status_code == 200, response.text
    value = response.json()
    physical = [source for source in value['source_map'] if source.get('evidence_kind') == 'physical_pdf_page']
    assert len(physical) == 1 and physical[0]['doc_id'] == doc and physical[0]['page'] == 1
    assert physical[0]['source_sha256'] == hashlib.sha256(original).hexdigest()
    assert pdf.read_bytes() == original
    assert any(tool['tool'] == 'read_pdf_source' and tool['status'] == 'ok' for tool in value['artifact']['tool_results'])


def test_pdf_transfer_boundary_cannot_separate_a_quantity_from_its_row(native_turn):
    t = native_turn
    name = 'Late boundary station with a deliberately long unique name exceeding one transfer'
    drawing = f'BT /F1 4 Tf 20 250 Td (|{name}|pcs|2|) Tj ET'.encode('ascii')
    path, digest = source_pdf(t, [drawing])
    original = path.read_bytes()
    def answer(body):
        text = '\n'.join(m.get('content') or '' for m in body['messages'])
        match = re.search(r'(S\d+R\d+) C1: ' + re.escape(name) + r'; C2: pcs; C3: 2', text)
        assert match, 'The model did not receive the complete row with its quantity: ' + text
        return {'text': json.dumps({'answerable': True, 'rows': [match[1]], 'sums': []})}
    t.scripts.extend([{'tool': 'read_pdf_source', 'args': {'doc_id': 'registered-document', 'page': 1, 'max_chars': 37}}, answer])
    response = t.client.post(PATH, headers=HEADERS, json={'question': 'Read the original quantity for the late boundary station.',
        'dataset_ids': ['ds'], 'mode': 'search', 'retrieval_mode': 'pull'})
    assert response.status_code == 200, response.text
    value = response.json()
    parts = [r['result'] for r in value['artifact']['tool_results'] if r['tool'] == 'read_pdf_source']
    assert len(parts) > 1 and len(t.seen) == 2
    assert sum(len(p['text']) for p in parts) == parts[0]['total_chars']
    assert len(value['source_map']) == 1
    source = value['source_map'][0]
    assert source['source_sha256'] == digest and source['page'] == 1
    assert source['page_text_complete'] and source['assembled_part_count'] == len(parts)
    assert value['artifact']['table_facts']['rows'][0]['cells'] == [name, 'pcs', '2']
    assert path.read_bytes() == original
