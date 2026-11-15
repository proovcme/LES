"""The public source path groups real storage, reads originals and keeps pins."""
import hashlib
import pytest
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


@pytest.fixture(autouse=True)
def overview_copy(monkeypatch,request):
    monkeypatch.setenv('LES_EXPERIMENT_EVIDENCE_PACKET','compact-v1')
    monkeypatch.setenv('LES_EXPERIMENT_OVERVIEW','documents-v2')
    monkeypatch.setenv('LES_EXPERIMENT_SAVED_READING','canonical-v1')
    monkeypatch.setenv('LES_EXPERIMENT_OVERVIEW_ADMISSION',getattr(request,'param','baseline-v1'))


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
    assert reply['result']['overview']['fusion_unit']=='document'
    assert reply['result']['evidence_packet']=='les.evidence-packet.v1'
    assert {source['source_sha256'] for source in reply['sources']}==hashes
    assert {source['dataset_id'] for source in reply['sources']}==set(datasets)
    assert all(set(p['source'])=={'ref'} for p in reply['result']['passages'])
    for source in reply['sources']:
        original=t.client.get(source['url'].split('#')[0]).raise_for_status().content
        assert hashlib.sha256(original).hexdigest()==source['source_sha256']
    assert not t.seen
    for view in ['overview','context']:
        args['view']=view;args['offset']=1
        for pin in [None,reply['result']['index_revision']]:
            if pin is None:args.pop('index_revision',None)
            else:args['index_revision']=pin
            rejected=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':args})
            assert rejected.status_code==422 and rejected.json()['code']=='SOURCE_SNAPSHOT_OFFSET_INVALID'
            assert 'read_source' in rejected.json()['message']


@pytest.mark.parametrize('overview_copy',['ready-v1'],indirect=True)
@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_ready_overview_shares_pdf_text_but_revalidates_every_original(native_turn,tmp_path):
    from contextlib import closing
    from qdrant_client import QdrantClient,models
    from tests.pdf_fixtures import write_pdf
    t=native_turn;folder=tmp_path/'documents';folder.mkdir()
    for name in ['a.pdf','b.pdf']:
        write_pdf(folder/name,['Equipment overview Pump A capacity 17.25 kW.'])
    dataset=t.client.post('/api/rag/datasets',json={'name':'Ready evidence sources'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=60)
    import sqlite3
    with sqlite3.connect(t.database) as db:
        db.row_factory=sqlite3.Row
        docs=[dict(row) for row in db.execute('SELECT id,file_name,file_hash FROM documents WHERE dataset_id=?',(dataset,))]
    first=next(d for d in docs if d['file_name'].endswith('a.pdf'))
    raw=t.client.post('/api/rag/sources/call',json={'tool':'read_pdf_source','args':{
        'doc_id':first['id'],'source_sha256':hashlib.sha256((folder/'a.pdf').read_bytes()).hexdigest(),'page':1,'max_chars':12000}}).raise_for_status().json()['result']['text']
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'],api_key=t.env['LES_LIGHT_QDRANT_API_KEY'],check_compatibility=False)) as client:
        collection=client.get_collections().collections[0].name
        scope=models.Filter(must=[models.FieldCondition(key='dataset_id',match=models.MatchValue(value=dataset))])
        # Both fixture representations contain text read from the actual PDF.
        # Only one requests physical hydration, reproducing mixed old/new views.
        client.set_payload(collection,payload={'text':raw},points=scope,wait=True)
        client.delete_payload(collection,keys=['pdf_page_view','pdf_page_view_schema','pdf_page_view_sha256'],points=scope,wait=True)
        # Request a physical reread through the legacy scan locator. The real
        # reader can use the clean PDF text; no differing table warning is added.
        client.set_payload(collection,payload={'pdf_requires_ocr':True},points=models.Filter(must=scope.must+[
            models.FieldCondition(key='file_name',match=models.MatchValue(value=first['file_name']))]),wait=True)
    args={'q':'Equipment Pump capacity overview','dataset_ids':[dataset],'view':'overview','max_chars':12000}
    answer=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':args}).raise_for_status().json()
    assert answer['result']['ready_evidence']['shared_bodies']>=1
    assert len(answer['result']['passages'])==1
    assert answer['result']['overview']['delivered_documents']==2
    source=answer['sources'][0];origins=[source]+source['equivalent_sources']
    assert {s['doc_id'] for s in origins}=={d['id'] for d in docs}
    for s in origins:
        data=t.client.get(s['url'].split('#')[0]).raise_for_status().content
        assert hashlib.sha256(data).hexdigest()==s['source_sha256']
    assert not t.seen
    write_pdf(folder/'b.pdf',['Changed original capacity 117.25 kW.'])
    changed=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':args})
    assert changed.status_code==409
    assert changed.json()['code'] in ('SOURCE_REVISION_CHANGED','PDF_REVISION_CHANGED')
    assert not t.seen
