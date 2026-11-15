"""Opt-in compact transport retains real native source reading and revision guards."""
import hashlib
import pytest
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


@pytest.fixture(autouse=True)
def packet_trial(monkeypatch):
    monkeypatch.setenv('LES_EXPERIMENT_CONTEXT_ADMISSION', 'baseline-v1')
    monkeypatch.setenv('LES_EXPERIMENT_EVIDENCE_PACKET', 'compact-v1')


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_compact_discovery_reads_pinned_original_and_rejects_replacement(native_turn, tmp_path):
    from docx import Document
    t = native_turn
    folder = tmp_path / 'documents'; folder.mkdir()
    path = folder / 'Specification.docx'
    doc = Document(); doc.add_heading('Equipment', level=1)
    table = doc.add_table(rows=3, cols=2)
    for row, values in zip(table.rows, [['Item', 'Power kW'], ['Pump A', '17.25'], ['Pump B', '117.250']]):
        for cell, value in zip(row.cells, values): cell.text = value
    doc.save(path); sha = hashlib.sha256(path.read_bytes()).hexdigest()
    dataset = t.client.post('/api/rag/datasets', json={'name': 'Packet control'}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external', json={'path': str(folder), 'dataset_id': dataset, 'background': True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda: t.client.get('/api/light/intake', params={'id': job}).json().get('status') == 'completed', timeout=60)
    args = {'q': 'Equipment pump power', 'view': 'context', 'dataset_ids': [dataset], 'max_chars': 12000}
    reply = t.client.post('/api/rag/sources/call', json={'tool': 'search_sources', 'args': args}).raise_for_status().json()
    assert reply['result']['evidence_packet'] == 'les.evidence-packet.v1'
    assert reply['result']['passages']
    texts = []
    for passage in reply['result']['passages']:
        assert set(passage['source']) == {'ref'}
        source = reply['sources'][passage['source']['ref'] - 1]
        assert source['source_sha256'] == sha and source['dataset_id'] == dataset
        assert sha in source['url']; texts.append(passage['text'])
        original = t.client.get(source['url'].split('#')[0]).raise_for_status().content
        assert hashlib.sha256(original).hexdigest() == sha
    assert '17.25' in '\n'.join(texts) and '117.250' in '\n'.join(texts)
    assert not t.seen, 'No answer model may be used by source tools'
    replacement = Document(); replacement.add_paragraph('Changed original'); replacement.save(path)
    stale = t.client.post('/api/rag/sources/call', json={'tool': 'search_sources', 'args': args})
    assert stale.status_code == 409 and stale.json()['code'] == 'SOURCE_REVISION_CHANGED'
