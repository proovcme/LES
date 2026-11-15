"""The public source path groups real storage, reads originals and keeps pins."""
import hashlib
import pytest
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


@pytest.fixture(autouse=True)
def overview_copy(monkeypatch):
    monkeypatch.setenv('LES_EXPERIMENT_EVIDENCE_PACKET','compact-v1')
    monkeypatch.setenv('LES_EXPERIMENT_OVERVIEW','grouped-v1')
    monkeypatch.setenv('LES_EXPERIMENT_SAVED_READING','canonical-v1')


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_overview_public_tool_keeps_same_filename_in_two_datasets_and_original_links(native_turn,tmp_path):
    from docx import Document
    t=native_turn;datasets=[];hashes=set()
    for index in range(2):
        folder=tmp_path/f'sources-{index}';folder.mkdir()
        for name in ['same.docx','independent.docx']:
            doc=Document();doc.add_heading('Equipment overview',level=1)
            doc.add_paragraph(f'Equipment query source {index} {name} capacity {17+index}.25 kW.')
            path=folder/name;doc.save(path);hashes.add(hashlib.sha256(path.read_bytes()).hexdigest())
        dataset=t.client.post('/api/rag/datasets',json={'name':f'Overview scope {index}'}).raise_for_status().json()['id']
        datasets.append(dataset)
        job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
        def completed():
            status=t.client.get('/api/light/intake',params={'id':job}).json()
            assert status.get('status') not in ('failed','cancelled'),status
            return status.get('status')=='completed'
        wait_for(completed,timeout=60)
    args={'q':'Equipment query capacity overview','dataset_ids':datasets,'view':'overview','max_chars':12000}
    reply=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':args}).raise_for_status().json()
    assert reply['result']['context_policy']=='document-coverage-v1'
    assert reply['result']['overview']['complete_corpus'] is False
    assert reply['result']['evidence_packet']=='les.evidence-packet.v1'
    assert {source['source_sha256'] for source in reply['sources']}==hashes
    assert {source['dataset_id'] for source in reply['sources']}==set(datasets)
    assert all(set(p['source'])=={'ref'} for p in reply['result']['passages'])
    for source in reply['sources']:
        original=t.client.get(source['url'].split('#')[0]).raise_for_status().content
        assert hashlib.sha256(original).hexdigest()==source['source_sha256']
    assert not t.seen
    args['offset']=1
    rejected=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':args})
    assert rejected.status_code==422 and rejected.json()['code']=='SOURCE_OFFSET_INVALID'
