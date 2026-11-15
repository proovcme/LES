"""The explicit RRF ablation preserves original reading and cursor guards."""
import hashlib
import pytest
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


@pytest.fixture(autouse=True)
def context_trial(monkeypatch):
    monkeypatch.setenv('LES_EXPERIMENT_CONTEXT_ADMISSION','rrf-wire-v1')


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_ranked_context_keeps_pinned_sources_and_an_unbroken_word_stream(native_turn,tmp_path):
    from docx import Document
    t=native_turn;folder=tmp_path/'originals';folder.mkdir();path=folder/'source.docx'
    doc=Document();doc.add_heading('Equipment evidence',level=1)
    for n in range(12):doc.add_paragraph(f'PART_{n:02} controlled equipment evidence has quantity {n}.250 pcs.')
    table=doc.add_table(rows=3,cols=2)
    for row,values in zip(table.rows,[['Item','Power'],['Pump A','17.25 kW'],['Pump B','117.250 kW']]):
        for cell,value in zip(row.cells,values):cell.text=value
    doc.save(path);sha=hashlib.sha256(path.read_bytes()).hexdigest()
    dataset=t.client.post('/api/rag/datasets',json={'name':'RRF control'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=60)
    reply=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':{
        'q':'Equipment evidence and pump power','view':'context','dataset_ids':[dataset],'max_chars':12000}}).raise_for_status().json()
    result=reply['result'];assert result['context_policy']=='rrf-wire-v1'
    assert result['passages'] and sum(len(p['text']) for p in result['passages'])<=12000
    for p in result['passages']:
        source=p['source'];assert source['dataset_id']==dataset and source['source_sha256']==sha
        assert sha in source['url']
    source=result['passages'][0]['source']
    args={'doc_id':source['doc_id'],'source_sha256':sha,'max_chars':12000,'limit':1,'offset':0}
    ordinals=[];texts=[];view=None
    for _ in range(64):
        read=t.client.post('/api/rag/sources/call',json={'tool':'read_source','args':args}).raise_for_status().json()['result']
        view=view or read['view_sha256'];assert read['view_sha256']==view
        passages=read['passages'];assert passages
        ordinals.extend(p['source']['ordinal'] for p in passages);texts.extend(p['text'] for p in passages)
        if read['next_offset'] is None:break
        assert read['next_offset']==args['offset']+len(passages)
        args.update(offset=read['next_offset'],view_sha256=view)
        # Original Word views can be read without an index. A null revision
        # in that response is not a valid string selector in the next request.
        if read.get('index_revision'):args['index_revision']=read['index_revision']
    else:pytest.fail('The original stream did not reach its end')
    assert ordinals==list(range(len(ordinals)))
    for n in range(12):assert sum(text.count(f'PART_{n:02}') for text in texts)==1
    assert '17.25' in '\n'.join(texts) and '117.250' in '\n'.join(texts)
    assert not t.seen,'Source tools must not call an answer model'
    replacement=Document();replacement.add_paragraph('Changed source.');replacement.save(path)
    changed=t.client.post('/api/rag/sources/call',json={'tool':'search_sources','args':{
        'q':'Equipment evidence','view':'context','dataset_ids':[dataset],'max_chars':12000}})
    assert changed.status_code==409 and changed.json()['code']=='SOURCE_REVISION_CHANGED'
