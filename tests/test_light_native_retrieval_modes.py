"""The three chat strategies share the index and deliver grounded source maps."""
from datetime import datetime, timezone
import json
import re
import os
import hashlib
import sqlite3
import time

import pytest

from proxy.services.model_connection_contracts import (
    CapabilityName, CapabilityObservation, CapabilitySnapshot, CapabilityState, ConnectionLocality, ConnectionRole,
)
from tests.test_light_native_api import wait_for
from tests.test_light_native_chat_turn import native_turn
from tests.pdf_fixtures import write_pdf


def source_excerpt_reply(body):
    """A numeric mock selects delivered evidence instead of inventing its values."""
    text=json.dumps(body['messages'],ensure_ascii=False)
    reference=re.search(r'\[Фрагмент (S\d+)\]',text)
    assert reference, 'No admitted excerpt reached the model'
    return {'text':json.dumps({'answerable':True,'rows':[reference[1]],'sums':[]})}


def test_source_api_distinguishes_unready_index_from_empty_evidence_without_calling_chat(native_turn):
    t=native_turn
    name='Same display name is not an ID'
    dataset=t.client.post('/api/rag/datasets',json={'name':name}).raise_for_status().json()['id']
    route='/api/rag/sources/call'
    result=t.client.post(route,json={'tool':'search_sources','args':{'q':'query','dataset_ids':[dataset]}})
    assert result.status_code==503 and result.json()['code']=='SOURCE_INDEX_NOT_READY'
    catalog=t.client.post(route,json={'tool':'search_sources','args':{'q':'query','view':'documents','dataset_ids':[dataset]}}).raise_for_status()
    assert catalog.headers['x-les-handler']=='rust.sources'
    assert catalog.json()['result']['documents']==[] and catalog.json()['sources']==[]
    rejected=t.client.post(route,json={'tool':'search_sources','args':{'q':'query','dataset_ids':[name]}})
    assert rejected.status_code==403 and rejected.json()['code']=='TOOL_SCOPE_VIOLATION'
    assert not t.seen, 'Source discovery must not request an answer model'


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_mixed_page_prose_can_be_selected_without_copying_an_unrelated_table(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    first='MIXED_TEXT_ANCHOR The requested location has capacity 17.35 kW.'
    (folder/'mixed.md').write_text(first+'\n\n|Equipment|Quantity|\n|---|---|\n|Unrelated pump|29|\n\nMIXED_TEXT_ANCHOR A separate continuation has length 42.75 m.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Mixed source evidence'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def answer(body):
        text=json.dumps(body['messages'],ensure_ascii=False)
        found=re.search(r'(S\d+P\d+) MIXED_TEXT_ANCHOR The requested location',text)
        return {'text':json.dumps({'answerable':True,'rows':[found[1] if found else 'S1P3'],'sums':[]})}
    t.scripts.append(answer)
    response=t.client.post('/api/chat',json={'question':'Find the MIXED_TEXT_ANCHOR location capacity.','mode':'search','retrieval_mode':'push',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}})
    assert response.status_code==200,response.text
    result=response.json()
    assert first in result['answer'] and 'Unrelated pump' not in result['answer'] and '42.75' not in result['answer']
    proof=result['artifact']['table_facts']['rows'][0]
    assert proof['kind']=='source_excerpt' and proof['paragraph_line']>0 and 'P' in proof['address']
    assert result['source_map'][0]['quote']==proof['cells'][0]


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
@pytest.mark.parametrize('query',[True,False])
def test_read_source_admits_relevance_before_presentation_order(native_turn,tmp_path,query):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    write_pdf(folder/'source.pdf',['ANCHOR background. '+('boring '*35),
        'ANCHOR BOUNDARY_CONFIRMED late value 17.35 kW. '+('proof '*22)])
    dataset=t.client.post('/api/rag/datasets',json={'name':'Reader admission'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    with sqlite3.connect(t.database) as db:doc=db.execute('SELECT id FROM documents WHERE dataset_id=?',[dataset]).fetchone()[0]
    args={'doc_id':doc,'limit':16,'max_chars':350}
    if query:args['q']='ANCHOR BOUNDARY_CONFIRMED'
    t.scripts.extend([{'tool':'read_source','args':args},source_excerpt_reply])
    result=t.client.post('/api/chat',json={'question':'Read the requested source passage.','mode':'search','retrieval_mode':'pull',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    passages=result['artifact']['tool_results'][0]['result']['passages']
    assert len(passages)==1
    assert passages[0]['source']['page']==(2 if query else 1)
    assert ('BOUNDARY_CONFIRMED' in result['answer'])==query
    assert result['source_map'][0]['page']==(2 if query else 1)
    assert len(t.seen)==2


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_initial_candidate_coverage_is_visible_without_changing_rrf_scores(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    (folder/'first.txt').write_text('ANCHOR meter north controlled value 17.35 kW.',encoding='utf-8')
    (folder/'second.txt').write_text('ANCHOR meter south controlled value 29.50 kW.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Independent retriever coverage'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    t.scripts.append(source_excerpt_reply)
    result=t.client.post('/api/chat',json={'question':'Find the ANCHOR meter values.','mode':'search','retrieval_mode':'hybrid',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    prefetch=result['retrieval_trace']['prefetch']
    assert prefetch['fusion']=='rrf'
    trace=prefetch['fusion_trace'];assert trace
    heads=[p['id'] for p in trace if any(c['rank']==1 for c in p['provenance']['channels'])]
    assert set(heads)<={p['id'] for p in trace[:2]}
    assert [p['provenance']['selection_rank'] for p in trace]==list(range(1,len(trace)+1))
    for p in trace:
        assert p['provenance']['head_coverage']=='complete'
        expected=sum(1/(10+c['rank']-1) for c in p['provenance']['channels'])
        assert p['score']==pytest.approx(expected)
    assert result['artifact']['validation']=='source_rows_verified'


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_releasing_schema_space_does_not_turn_qualitative_reading_into_a_table_task(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    (folder/'proof.txt').write_text('QUALITATIVE_ANCHOR controlled qualitative evidence.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Qualitative continuation'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    with sqlite3.connect(t.database) as db:doc=db.execute('SELECT id FROM documents WHERE dataset_id=?',[dataset]).fetchone()[0]
    def answer(body):
        assert not body.get('tools')
        text=json.dumps(body['messages'],ensure_ascii=False)
        assert 'controlled qualitative evidence' in text
        assert 'table_selection_error' not in text
        assert 'верни JSON rows' not in text
        return {'text':'Controlled qualitative evidence [Источник 1]'}
    t.scripts.extend([{'tool':'read_source','args':{'doc_id':doc,'q':'QUALITATIVE_ANCHOR '*150,'max_chars':400}},answer])
    response=t.client.post('/api/chat',json={'question':'Read QUALITATIVE_ANCHOR. '+('Preserve the current source context. '*95),
        'mode':'search','retrieval_mode':'pull','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}})
    assert response.status_code==200,response.text
    result=response.json()
    assert len(t.seen)==2 and result['retrieval_trace']['answer_window_retry'] is True
    assert result['retrieval_trace']['contexts'][-1]['table_selection'] is False
    assert result['answer']=='Controlled qualitative evidence [Источник 1]'


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_actual_read_continuation_can_close_schema_space_before_provider_request(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    (folder/'proof.txt').write_text('BUDGET_ANCHOR controlled value 17.35 kW.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Actual continuation budget'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    with sqlite3.connect(t.database) as db:doc=db.execute('SELECT id FROM documents WHERE dataset_id=?',[dataset]).fetchone()[0]
    query='BUDGET_ANCHOR '*180
    def answer(body):
        assert not body.get('tools')
        assert 'BUDGET_ANCHOR controlled value 17.35 kW.' in json.dumps(body['messages'])
        return source_excerpt_reply(body)
    t.scripts.extend([{'tool':'read_source','args':{'doc_id':doc,'q':query,'max_chars':400}},answer])
    result=t.client.post('/api/chat',json={'question':'Read BUDGET_ANCHOR. '+('Preserve the current source context. '*95),
        'mode':'search','retrieval_mode':'pull','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert len(t.seen)==2 and not t.scripts
    assert result['retrieval_trace']['answer_window_retry'] is True
    # Empty [] tool framing still costs one token; no schema remains.
    assert result['retrieval_trace']['contexts'][-1]['schema_reserved_tokens']==1
    assert result['artifact']['validation']=='source_rows_verified'
    recorded=result['artifact']['tool_results'][0]['inputs'][0]['arguments']['q']
    assert recorded=={'prefix':query[:2048],'sha256':hashlib.sha256(query.encode()).hexdigest(),'truncated':True}
    assert all(c['context']['included_tokens']<=c['context']['input_budget_tokens'] for c in result['retrieval_trace']['contexts'])


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_search_reads_a_fitting_document_across_page_parents_without_a_file_hint(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'documents';folder.mkdir()
    # Each page is a distinct parent. A fitting document must survive context
    # admission without early per-file seed caps or a filename in the question.
    pages=[f'ANCHOR pressure schedule. Physical sheet {i}. Measured value TOKEN_{i:02d}.' for i in range(1,13)]
    write_pdf(folder/'schedule.pdf',pages)
    dataset=t.client.post('/api/rag/datasets',json={'name':'Page boundaries'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def answer(body):
        text='\n'.join(m['content'] for m in body['messages'])
        assert all(f'TOKEN_{i:02d}' in text for i in range(1,13)), 'Fitting continuation pages were discarded'
        return {'text':'Schedule read [Источник 1]'}
    t.scripts.append(answer)
    result=t.client.post('/api/chat',json={'question':'Read the ANCHOR pressure schedule and its continuation.',
        'mode':'search','retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert {s['page'] for s in result['source_map']}==set(range(1,13))
    assert all(f"#page={s['page']}" in s['url'] for s in result['source_map'])
    assert len(t.seen)==1 and not result['artifact']['tool_results']
    policy=result['retrieval_trace']['prefetch']['selection_policy']
    assert policy['navigation_requested'] is False
    assert policy['seed_limit']==64 and policy['per_document_seed_limit'] is None
    assert policy['eligible_candidates']>=12 and policy['hydrated_candidates']>=12
    assert all(c['context']['included_tokens']<=c['context']['input_budget_tokens'] for c in result['retrieval_trace']['contexts'])


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_document_expansion_rejects_a_continuation_from_a_different_revision(native_turn,tmp_path):
    from contextlib import closing
    from qdrant_client import QdrantClient,models
    t=native_turn;folder=tmp_path/'documents';folder.mkdir()
    write_pdf(folder/'schedule.pdf',[f'ANCHOR schedule page {i}. Controlled value {i}.' for i in range(1,13)])
    dataset=t.client.post('/api/rag/datasets',json={'name':'Pinned continuation'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'],api_key=t.env['LES_LIGHT_QDRANT_API_KEY'],check_compatibility=False)) as client:
        collection=client.get_collections().collections[0].name
        rows,_=client.scroll(collection,scroll_filter=models.Filter(must=[models.FieldCondition(key='dataset_id',match=models.MatchValue(value=dataset))]),limit=100)
        tail=[r.id for r in rows if r.payload.get('page')==12 and r.payload.get('node_role')!='navigation']
        assert tail
        client.set_payload(collection,payload={'source_sha256':'0'*64},points=tail,wait=True)
    response=t.client.post('/api/chat',json={'question':'Read the ANCHOR schedule and its continuation.',
        'mode':'search','retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}})
    assert response.status_code==503 and response.json()['code']=='INDEX_CHANGED_DURING_SEARCH'
    assert not t.seen
    with sqlite3.connect(t.database) as db:
        answer,artifact=db.execute('SELECT answer,artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()
    assert answer=='' and json.loads(artifact)['error']['code']=='INDEX_CHANGED_DURING_SEARCH'


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_fitting_continuation_is_not_cut_at_a_fixed_six_thousand_characters(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'documents';folder.mkdir()
    pages=[f'WINDOW_ANCHOR schedule page {i}. '+('Original document content. '*27)+f' END_TOKEN_{i:02d}.' for i in range(1,11)]
    assert 6000<sum(map(len,pages))<12000
    write_pdf(folder/'schedule.pdf',pages)
    dataset=t.client.post('/api/rag/datasets',json={'name':'Model-sized reading window'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def answer(body):
        text=json.dumps(body['messages'])
        assert all(f'END_TOKEN_{i:02d}' in text for i in range(1,11))
        return {'text':'Whole fitting schedule read [Источник 1]'}
    t.scripts.append(answer)
    result=t.client.post('/api/chat',json={'question':'Read the WINDOW_ANCHOR schedule and its continuation.',
        'mode':'search','retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert {s['page'] for s in result['source_map']}==set(range(1,11))
    assert len(t.seen)==1 and not result['artifact']['tool_results']
    assert all(c['context']['included_tokens']<=c['context']['input_budget_tokens'] for c in result['retrieval_trace']['contexts'])


def source_read(t, dataset, suffix):
    with sqlite3.connect(t.database) as db:
        document=db.execute('SELECT id FROM documents WHERE dataset_id=? AND file_name LIKE ?',
                            [dataset,'%'+suffix]).fetchone()[0]
    return {'tool':'read_source','args':{'doc_id':document,'limit':1}}


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_unread_documents_remain_reachable_without_a_separate_model_selector(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    for i in range(12):
        (folder/f'source_{i}.txt').write_text(f'POOL_ANCHOR original record. CONTENT_ONLY_{i}_END.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Pool navigation'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    chosen={}
    def read_later_document(body):
        delivered=json.dumps(body['messages'])
        assert 'SOURCE_DOCUMENT_SELECTION' not in delivered
        chosen['initial_markers']={i for i in range(12) if f'CONTENT_ONLY_{i}_END' in delivered}
        for message in body['messages']:
            content=message.get('content') or ''
            for marker in re.finditer(r'"candidate_documents"\s*:',content):
                cards,_=json.JSONDecoder().raw_decode(content[marker.end():].lstrip())
                if not isinstance(cards,dict):continue
                assert cards['evidence_admissible'] is False and cards['total']==12
                assert len(cards['documents'])==12
                chosen.update(cards['documents'][-1])
                label=chosen['title'].rsplit(' / ',1)[-1]
                chosen['number']=int(re.fullmatch(r'source_(\d+)\.txt',label)[1])
                return {'tool':'read_source','args':{'doc_id':chosen['doc_id']}}
        raise AssertionError('The searched document pool was hidden from the reader')
    def answer(body):
        text='\n'.join(m.get('content') or '' for m in body['messages'])
        number=chosen['number']
        assert f'CONTENT_ONLY_{number}_END' in text
        assert all(f'CONTENT_ONLY_{i}_END' in text for i in chosen['initial_markers'])
        assert all(f'CONTENT_ONLY_{i}_END' not in text for i in range(12) if i!=number and i not in chosen['initial_markers'])
        fragments=re.split(r'\[Фрагмент (S\d+)\]',text)
        selected=next(fragments[i] for i in range(1,len(fragments),2) if f'CONTENT_ONLY_{number}_END' in fragments[i+1])
        return {'text':json.dumps({'answerable':True,'rows':[selected],'sums':[]})}
    before=len(t.embeddings.calls)
    t.scripts.extend([read_later_document,answer])
    result=t.client.post('/api/chat',json={'question':'Read the POOL_ANCHOR original record.',
        'mode':'search','retrieval_mode':'hybrid','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert len(t.seen)==2 and len(t.embeddings.calls)==before+1
    assert len(result['retrieval_trace']['prefetch']['candidate_documents'])==12
    assert chosen['doc_id'] in {s['doc_id'] for s in result['source_map']}
    assert len(result['source_map'])==len(chosen['initial_markers']|{chosen['number']})
    assert {r['doc_id'] for r in result['artifact']['table_facts']['rows']}=={chosen['doc_id']}
    assert [r['tool'] for r in result['artifact']['tool_results']]==['read_source']


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_tool_budget_allows_bounded_passages_with_their_citation_and_receipt_arrays(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    for i in range(9):
        (folder/f'{i}.txt').write_text(f'RECEIPT_POOL original record {i}.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Bounded result identities'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    t.scripts.extend([{'tool':'search_sources','args':{'q':'RECEIPT_POOL','limit':9,'max_chars':4000}},
                      {'text':'Bounded original records [Источник 1]'}])
    result=t.client.post('/api/chat',json={'question':'Read RECEIPT_POOL originals.',
        'mode':'search','retrieval_mode':'pull','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    raw=result['artifact']['tool_results'][0]
    assert raw['status']=='ok' and raw['execution']['code']=='TOOL_OK'
    assert len(raw['result']['passages'])==9 and len(raw['sources'])==9
    assert len(raw['result']['read_receipt']['point_ids'])==9
    # Each factual hit also has a source, a receipt ID and a navigation card.
    nested_items=sum(len(raw['result'][key]) for key in ('passages','candidate_documents'))+len(raw['sources'])+len(raw['result']['read_receipt']['point_ids'])
    assert nested_items>32
    assert len(t.seen)==2 and len(result['source_map'])==9


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
@pytest.mark.parametrize('mode',['search','agent'])
@pytest.mark.parametrize('numeric_error',[None,'repair','repeat'])
def test_hybrid_finishes_from_a_larger_answer_window_before_committing_partial_table_data(native_turn,tmp_path,mode,numeric_error):
    t=native_turn;folder=tmp_path/'documents';folder.mkdir()
    pages=['CONTROL_SCHEDULE\n'+'\n'.join(f'| CONTROL_PAGE_{letter} | m | 7 |' for _ in range(20)) for letter in 'ABCD']
    write_pdf(folder/'schedule.pdf',pages)
    dataset=t.client.post('/api/rag/datasets',json={'name':'Answer window'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def select_partial(body):
        text=json.dumps(body['messages'],ensure_ascii=False)
        assert body['tools'] and not all(f'CONTROL_PAGE_{letter}' in text for letter in 'ABCD')
        return {'text':'{"answerable":false,"rows":[],"sums":[]}'}
    def select_complete(body):
        text='\n'.join(m['content'] for m in body['messages'])
        assert not body.get('tools') and all(f'CONTROL_PAGE_{letter}' in text for letter in 'ABCD')
        chosen=[]
        for letter in 'AD':
            chosen.append(next(re.match(r'(S\d+R\d+) ',line)[1] for line in text.splitlines()
                               if re.match(r'(S\d+R\d+) ',line) and f'CONTROL_PAGE_{letter}' in line))
        return {'text':json.dumps({'answerable':True,'rows':chosen,'sums':[]})}
    def invalid_sum(body):
        selected=select_complete(body)
        value=json.loads(selected['text']);value['sums']=[{'column':999,'unit_column':None}]
        return {'text':json.dumps(value)}
    def correction(body):
        text=json.dumps(body['messages'],ensure_ascii=False)
        assert 'TABLE_VALUE_INVALID' in text and '"column": 999' not in text
        return invalid_sum(body) if numeric_error=='repeat' else select_complete(body)
    t.scripts.extend([select_partial,invalid_sum,correction] if numeric_error else [select_partial,select_complete])
    session=t.client.post('/api/workspace/sessions',json={'title':'Saved context window'}).raise_for_status().json()['session_id']
    response=t.client.post('/api/chat',json={'question':'Read CONTROL_SCHEDULE quantities. '+('Preserve original reading context. '*90),
        'session_id':session,'mode':mode,'retrieval_mode':'hybrid','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}})
    assert len(t.seen)==(3 if numeric_error else 2) and not t.scripts, response.text
    if numeric_error=='repeat':
        assert response.status_code==503 and response.json()['code']=='TABLE_VALUE_INVALID'
        with sqlite3.connect(t.database) as db:
            answer,success,artifact=db.execute('SELECT answer,success,artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()
        assert answer=='' and not success and json.loads(artifact)['error']['code']=='TABLE_VALUE_INVALID'
        return
    result=response.raise_for_status().json()
    assert result['retrieval_trace']['answer_window_retry'] is True
    assert result['artifact']['tool_results']==[] and result['artifact']['validation']=='source_rows_verified'
    cited={r['source_index'] for r in result['artifact']['table_facts']['rows']}
    assert {s['page'] for s in result['source_map'] if s['index'] in cited}=={1,4}
    assert all(c['context']['included_tokens']<=c['context']['input_budget_tokens'] for c in result['retrieval_trace']['contexts'])


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_passage_offsets_advance_ranked_hits_and_report_exhaustion(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    for name in ('alpha','beta','gamma'):
        (folder/f'{name}.txt').write_text(f'OFFSET_MARKER controlled {name} evidence. '*8,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Ranked search pages'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    t.scripts.extend([{'calls':[{'tool':'search_sources','args':{'q':'OFFSET_MARKER','limit':1,'offset':i}} for i in range(4)]},
                      {'text':'Retrieved controlled evidence [Источник 1]'}])
    result=t.client.post('/api/chat',json={'question':'Find OFFSET_MARKER documents.',
        'mode':'search','retrieval_mode':'pull','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    rows=[t['result'] for t in result['artifact']['tool_results']]
    assert [r['offset'] for r in rows]==[0,1,2,3]
    assert [r['total_candidates'] for r in rows]==[3]*4
    assert [r['next_offset'] for r in rows]==[1,2,None,None]
    assert len({r['passages'][0]['source']['doc_id'] for r in rows[:3]})==3
    assert rows[3]['passages']==[] and rows[3]['coverage']=='no_matches'
    assert len({r['index_revision'] for r in rows})==1
    assert len(t.seen)==2
    assert all('fusion_trace' not in row for row in rows)


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
@pytest.mark.parametrize('tool', ['read_source', 'search_sources'])
def test_source_cursor_covers_unequal_pages_once_and_stops_at_end(native_turn, tmp_path, tool):
    t = native_turn
    folder = tmp_path / 'source'; folder.mkdir()
    pages = ['CURSOR_MARKER ALPHA ' + 'source ' * 22 + 'END_ALPHA',
             'CURSOR_MARKER BETA ' + 'source ' * 22 + 'END_BETA',
             'CURSOR_MARKER GAMMA evidence END_GAMMA',
             'CURSOR_MARKER DELTA evidence END_DELTA']
    original = write_pdf(folder / 'ordered.pdf', pages)
    digest = hashlib.sha256(original.read_bytes()).hexdigest()
    dataset = t.client.post('/api/rag/datasets', json={'name': 'Unequal source pages'}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external', json={'path': str(folder), 'dataset_id': dataset, 'background': True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda: t.client.get('/api/light/intake', params={'id': job}).json().get('status') == 'completed', timeout=40)
    with sqlite3.connect(t.database) as db:
        doc = db.execute('SELECT id FROM documents WHERE dataset_id=?', [dataset]).fetchone()[0]
    from contextlib import closing
    from qdrant_client import QdrantClient
    from qdrant_client.http import models
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'], api_key=t.env['LES_LIGHT_QDRANT_API_KEY'], check_compatibility=False)) as index:
        rows, more = index.scroll('docs', scroll_filter=models.Filter(
            must=[models.FieldCondition(key='dataset_id', match=models.MatchValue(value=dataset))],
            must_not=[models.FieldCondition(key='node_role', match=models.MatchValue(value='navigation')),
                      models.FieldCondition(key='evidence_admissible', match=models.MatchValue(value=False))]),
            limit=100, with_payload=True, with_vectors=False)
    assert more is None and len(rows) == 4
    expected_ids = {str(row.id) for row in rows}
    offsets = []
    def step(body):
        replies = []
        for message in body['messages']:
            if message.get('role') == 'tool':
                value = json.loads(message['content'])
                if value.get('tool') == tool:
                    replies.append(value)
        if replies:
            last = replies[-1]['result']
            if last['next_offset'] is None:
                return source_excerpt_reply(body)
            offset = last['next_offset']
        else:
            offset = 0
        assert offset not in offsets and len(offsets) < 5
        offsets.append(offset)
        args = {'limit': 2, 'max_chars': 250, 'offset': offset}
        args.update({'doc_id': doc} if tool == 'read_source' else {'q': 'CURSOR_MARKER'})
        return {'tool': tool, 'args': args}
    t.scripts.extend([step] * 6)
    response = t.client.post('/api/chat', json={'question': 'Read CURSOR_MARKER source evidence.', 'mode': 'search', 'retrieval_mode': 'pull',
        'scope': {'scope_type': 'dataset', 'dataset_ids': [dataset], 'selected_sources_only': True}})
    assert response.status_code == 200, response.text
    result = response.json()
    receipts = [r['result'] for r in result['artifact']['tool_results'] if r['tool'] == tool]
    assert len(receipts) > 1 and receipts[-1]['next_offset'] is None
    assert [r['offset'] for r in receipts] == offsets
    passages = [p for r in receipts for p in r['passages']]
    identities = [p['source']['qdrant_point_id'] for p in passages]
    assert len(identities) == len(set(identities)) == 4
    assert set(identities) == expected_ids
    assert {p['source']['page'] for p in passages} == {1, 2, 3, 4}
    assert all(any(end in p['text'] for p in passages) for end in ['END_ALPHA', 'END_BETA', 'END_GAMMA', 'END_DELTA'])
    assert hashlib.sha256(original.read_bytes()).hexdigest() == digest
    assert all(r['next_offset'] == r['offset'] + len(r['passages']) for r in receipts[:-1])
    assert all(c['context']['included_tokens'] <= c['context']['input_budget_tokens'] for c in result['retrieval_trace']['contexts'])


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_push_supplies_verified_rust_context_without_model_tools(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'source';folder.mkdir()
    proof='Controlled source value 17.35 kW. '*8
    (folder/'proof.txt').write_text(proof,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Data-first search'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def answer(body):
        assert not body.get('tools') and proof.rstrip() in json.dumps(body['messages'])
        return source_excerpt_reply(body)
    t.scripts.append(answer)
    result=t.client.post('/api/chat',json={'question':'Controlled source value?','mode':'search','retrieval_mode':'push',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert len(t.seen)==1 and result['retrieval_trace']['mode']=='push'
    assert result['artifact']['tool_results']==[] and result['retrieval_trace']['prefetch']['read_receipt']['schema']=='les.indexed-read.v1'
    assert result['artifact']['validation']=='source_rows_verified' and '17.35' in result['source_map'][0]['quote']
    assert result['retrieval_trace']['contexts'][0]['schema_reserved_tokens']<10


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_source_inspection_does_not_mix_tool_selection_with_final_answer_formatting(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    (folder/'proof.txt').write_text('Controlled proof 17.35 kW. '*12,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Inspection stages'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def request_message(body):
        for m in reversed(body['messages']):
            if m['role']=='user':
                remaining=m['content'].lstrip()
                while remaining:
                    try:value,end=json.JSONDecoder().raw_decode(remaining)
                    except (ValueError,TypeError):break
                    if isinstance(value,dict) and 'question' in value:return value
                    remaining=remaining[end:].lstrip()
        raise AssertionError('Missing bound question')
    def select(body):
        question=request_message(body)
        assert question['stage']=='source_inspection' and question['source_inspection_required'] is True
        assert 'output_directive' not in question and 'response_length' not in question
        assert 'CURRENT STAGE: source inspection' in body['messages'][0]['content']
        return source_read(t,dataset,'proof.txt')
    def answer(body):
        question=request_message(body)
        assert 'stage' not in question and question['response_length']=='short'
        return source_excerpt_reply(body)
    t.scripts.extend([select,answer])
    response=t.client.post('/api/chat',json={'question':'Controlled proof?','mode':'search','retrieval_mode':'pull','response_length':'short',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert response['retrieval_trace']['source_inspection']['performed'] is True and len(t.seen)==2


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_sequential_reads_in_an_8k_window_get_bounded_portions_and_keep_durable_receipts(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'sources';folder.mkdir()
    for name in ('alpha','beta'):
        (folder/f'{name}.txt').write_text((f'{name} controlled reading. '+('Source paragraph. '*65)+'\n\n')*15,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Small window'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    reads=[source_read(t,dataset,name+'.txt') for name in ('alpha','beta')]
    for read in reads:read['args'].update(max_chars=16000,limit=16)
    def inspected(body):
        result=json.loads(next(m['content'] for m in reversed(body['messages']) if m.get('role')=='tool'))
        grant=result['result']['context_budget']
        assert grant['requested_max_chars']==16000 and 0<grant['granted_max_chars']<16000
        assert result['result']['coverage']=='bounded'
        index=result['result']['passages'][0]['source']['index']
        return {'text':f'Read the current beta source [Источник {index}]'}
    t.scripts.extend([*reads,inspected])
    result=t.client.post('/api/chat',json={'question':'Read the second source after the first','mode':'search','retrieval_mode':'pull',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert len(t.seen)==3
    assert all(c['context']['included_tokens']<=c['context']['input_budget_tokens'] for c in result['retrieval_trace']['contexts'])
    with sqlite3.connect(t.database) as db:
        artifact=json.loads(db.execute('SELECT artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()[0])
    receipts=artifact['tool_results']
    assert len(receipts)==2 and all(r['result']['passages'] and r['result']['context_budget']['requested_max_chars']==16000 for r in receipts)
    assert all(r['inputs'][0]['kind']=='source_request' and r['inputs'][0]['arguments']['max_chars']==r['result']['context_budget']['granted_max_chars'] for r in receipts)


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
@pytest.mark.parametrize('mode',['push','hybrid','pull'])
def test_dataset_metadata_does_not_authorize_a_grounded_answer_and_failure_keeps_its_reason(native_turn,tmp_path,mode):
    t=native_turn;folder=tmp_path/'read-control';folder.mkdir()
    (folder/'proof.txt').write_text('Controlled reading requirement 17.35 kW.',encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Read control'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    empty=t.client.post('/api/rag/datasets',json={'name':'Metadata without source text'}).raise_for_status().json()['id']
    if mode!='push':t.scripts.append({'text':'UNSUPPORTED CONFIDENT CLAIM [Источник 1]'})
    response=t.client.post('/api/chat',json={'question':'Controlled reading requirement?','mode':'search','retrieval_mode':mode,
        'scope':{'scope_type':'dataset','dataset_ids':[empty],'selected_sources_only':True}})
    assert response.status_code==503 and response.json()['code']=='SOURCE_EVIDENCE_MISSING'
    assert len(t.seen)==(0 if mode=='push' else 1) and not t.scripts
    with sqlite3.connect(t.database) as db:
        answer,success,artifact,trace=db.execute('SELECT answer,success,artifact_json,retrieval_trace_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()
    assert answer=='' and not success and json.loads(artifact)['error']['code']=='SOURCE_EVIDENCE_MISSING'
    if mode=='push':assert json.loads(trace)['reason']=='rust_context_has_no_verified_text'
    else:assert json.loads(trace)['grounding_retry']['sources']==0


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_catalog_discovery_is_paginated_scoped_and_not_evidence_or_an_embedding_call(native_turn,tmp_path):
    t=native_turn;datasets=[]
    for group in ('a','b'):
        folder=tmp_path/group;folder.mkdir()
        for i in range(2):(folder/f'Permit-{i}.txt').write_text(f'PRIVATE BODY {group}{i}. Power 17.35 kW.',encoding='utf-8')
        dataset=t.client.post('/api/rag/datasets',json={'name':group}).raise_for_status().json()['id'];datasets.append(dataset)
        job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
        wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    with sqlite3.connect(t.database) as db:
        doc,name=db.execute('SELECT id,file_name FROM documents WHERE dataset_id=? ORDER BY file_name LIMIT 1',[datasets[0]]).fetchone()
    def after_catalog(body):
        payloads=[json.loads(m['content']) for m in body['messages'] if m.get('role')=='tool']
        first,last=payloads[-2:]
        assert first['sources']==last['sources']==[] and first['result']['evidence_admissible'] is False
        assert first['result']['total']==4 and first['result']['next_offset']==2
        assert last['result']['next_offset'] is None
        assert len({d['id'] for p in (first,last) for d in p['result']['documents']})==4
        assert 'PRIVATE BODY' not in json.dumps(body)
        assert 'read_source' in [x['function']['name'] for x in body['tools']]
        return {'tool':'read_source','args':{'doc_id':doc}}
    t.scripts.extend([{'tool':'search_sources','args':{'q':'Permit','view':'documents','limit':2}},
                      {'tool':'search_sources','args':{'q':'Permit','view':'documents','offset':2,'limit':2}},
                      after_catalog,source_excerpt_reply])
    before=len(t.embeddings.calls)
    scope={'scope_type':'datasets','dataset_ids':datasets,'selected_sources_only':True}
    result=t.client.post('/api/chat',json={'question':'Read permit power','mode':'search','retrieval_mode':'pull','scope':scope}).raise_for_status().json()
    assert len(t.embeddings.calls)==before and result['retrieval_trace']['source_inspection']['performed'] is True
    assert result['source_map'][0]['doc_id']==doc
    def only_selected(body):
        result=json.loads(next(m['content'] for m in reversed(body['messages']) if m.get('role')=='tool'))
        assert result['result']['total']==1 and result['result']['documents'][0]['id']==doc
        return {'tool':'read_source','args':{'doc_id':doc}}
    t.scripts.extend([{'tool':'search_sources','args':{'q':'Permit','view':'documents'}},only_selected,{'text':'Read [Источник 1]'}])
    t.client.post('/api/chat',json={'question':'Read selected permit','mode':'search','retrieval_mode':'pull',
        'scope':scope,'target_files':[name]}).raise_for_status()
    t.scripts.extend([{'tool':'dataset_map','args':{'dataset_id':datasets[0]}},only_selected,{'text':'Read [Источник 1]'}])
    t.client.post('/api/chat',json={'question':'List and read only the selected permit','mode':'search','retrieval_mode':'pull',
        'scope':scope,'target_files':[name]}).raise_for_status()


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_ocr_provenance_survives_retrieval_and_warns_the_model(native_turn,tmp_path):
    from contextlib import closing
    from qdrant_client import QdrantClient,models
    from tests.test_light_native_intake_execution import settings
    t=native_turn;folder=tmp_path/'source';folder.mkdir()
    (folder/'proof.txt').write_text('OCR projection control 17.35 kW. '+('Controlled source text. '*10),encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'OCR provenance projection'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    # A controlled payload annotation tests projection only, not OCR accuracy.
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'],api_key=t.env['LES_LIGHT_QDRANT_API_KEY'],check_compatibility=False)) as client:
        client.set_payload(settings()['RAG_COLLECTION_NAME'],payload={'ocr':True,'visual_content':True,'unread_visual_count':1},points=models.Filter(must=[models.FieldCondition(key='dataset_id',match=models.MatchValue(value=dataset))]))
    t.scripts.append(source_excerpt_reply)
    result=t.client.post('/api/chat',json={'question':'OCR projection control?','mode':'search','retrieval_mode':'push',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert result['source_map'][0]['ocr'] is True
    assert 'Текст получен OCR' in json.dumps(t.seen[0]['messages'],ensure_ascii=False)
    assert result['source_map'][0]['visual_content'] is True
    assert 'Текстовая выдержка не подтверждает' in json.dumps(t.seen[0]['messages'],ensure_ascii=False)


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
@pytest.mark.parametrize('supplies_citation',[True,False])
def test_document_answer_gets_one_citation_revision_and_failure_is_saved(native_turn,tmp_path,supplies_citation):
    t=native_turn;folder=tmp_path/'source';folder.mkdir()
    (folder/'proof.txt').write_text('Controlled power 17.35 kW. '+('Verified original text. '*10),encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Citation format'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    # Isolate citation repair; numeric commits have their own stricter row contract.
    t.scripts.extend([{'text':'Verified value without a reference'},{'text':'Verified value [Источник 1]' if supplies_citation else 'Still no reference'}])
    response=t.client.post('/api/chat',json={'question':'Controlled power?','mode':'search','retrieval_mode':'push',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}})
    assert len(t.seen)==2 and all(not request.get('tools') for request in t.seen)
    assert 'citation_format_required' in json.dumps(t.seen[1]['messages'])
    if supplies_citation:
        result=response.raise_for_status().json()
        assert result['answer']=='Verified value [Источник 1]' and result['retrieval_trace']['citation_retry'] is True
    else:
        assert response.status_code>=400 and response.json()['code']=='SOURCE_CITATION_MISSING'
        with sqlite3.connect(t.database) as db:
            answer,success,artifact=db.execute('SELECT answer,success,artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()
        assert answer=='' and not success and json.loads(artifact)['error']['code']=='SOURCE_CITATION_MISSING'


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_hybrid_keeps_four_documents_before_factual_answer(native_turn,tmp_path):
    t=native_turn;folder=tmp_path/'source';folder.mkdir()
    proofs=[f'Control source number {i}. '+('Common measurement data. '*10) for i in range(4)]
    for i,proof in enumerate(proofs):(folder/f'{i}.txt').write_text(proof,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Hybrid breadth'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def answer(body):
        prompt=body['messages'][0]['content']
        assert 'На этом шаге выбирай инструменты' not in prompt
        assert 'Ответ без вызова допустим только на этапе ответа' not in prompt
        assert body.get('tools'), 'Optional refinement must remain available in Hybrid'
        return {'text':'Common measurement data. [Источник 1]'}
    t.scripts.append(answer)
    result=t.client.post('/api/chat',json={'question':'Common measurement data?','mode':'search','retrieval_mode':'hybrid',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    delivered=json.dumps(t.seen[0]['messages'])
    assert all(f'{i}.txt' in delivered for i in range(4))
    assert all(proof.rstrip() in delivered for proof in proofs)
    assert len(t.seen)==1 and result['artifact']['tool_results']==[]
    assert len(result['source_map'])==4
    assert result['retrieval_trace']['prefetch']['read_receipt']['schema']=='les.indexed-read.v1'
    assert result['retrieval_trace']['source_inspection']['performed'] is True


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_pull_recovers_one_ungrounded_completion_by_reading_the_selected_dataset(native_turn,tmp_path):
    t=native_turn
    folder=tmp_path/'source';folder.mkdir()
    proof='Controlled measurement 17.35 kW. '+('Source evidence retained. '*10)
    (folder/'source.txt').write_text(proof,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Grounding recovery'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    t.scripts.extend([{'text':'UNGROUNDED DRAFT MUST BE DISCARDED'},source_excerpt_reply])
    response=t.client.post('/api/chat',json={'question':'Controlled measurement?','retrieval_mode':'pull','mode':'search',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert len(t.seen)==2 and response['artifact']['tool_results']==[]
    assert proof not in json.dumps(t.seen[0]['messages'])
    assert proof.rstrip() in json.dumps(t.seen[1]['messages'])
    assert response['artifact']['validation']=='source_rows_verified' and proof.rstrip() in response['source_map'][0]['quote']
    assert response['source_map'][0]['dataset_id']==dataset
    assert response['retrieval_trace']['grounding_retry'] is True
    with sqlite3.connect(t.database) as db:
        saved=db.execute('SELECT answer FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()[0]
    assert saved==response['answer'] and 'UNGROUNDED' not in saved


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_due_watch_is_serviced_before_another_datasets_bulk_import_finishes(native_turn, tmp_path):
    t=native_turn
    folder=tmp_path/'watched';folder.mkdir()
    source=folder/'record.txt';source.write_text('Original controlled observation. '*25,encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Watched'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True,'watch':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    watch=t.client.get('/api/light/folder-watches').raise_for_status().json()['watches'][0]
    t.client.post('/api/light/folder-watches',json={'id':watch['id'],'enabled':False}).raise_for_status()
    bulk=tmp_path/'bulk';bulk.mkdir()
    for number in range(20):
        (bulk/f'{number:02}.txt').write_text(f'BULK-{number} '+('Background source data. '*70),encoding='utf-8')
    other=t.client.post('/api/rag/datasets',json={'name':'Bulk'}).raise_for_status().json()['id']
    t.embeddings.on_request=lambda _:time.sleep(.05)
    queued=t.client.post('/api/rag/index-external',json={'path':str(bulk),'dataset_id':other,'background':True,'parse_limit':1}).raise_for_status().json()['intake_job_id']
    source.write_text('Changed controlled observation. '*25,encoding='utf-8')
    expected=hashlib.sha256(source.read_bytes()).hexdigest()
    t.client.post('/api/light/folder-watches',json={'id':watch['id'],'enabled':True}).raise_for_status()
    def caught_up():
        with sqlite3.connect(t.database) as db:
            return db.execute('SELECT file_hash FROM documents WHERE dataset_id=?',(dataset,)).fetchone()[0]==expected
    wait_for(caught_up,timeout=30)
    assert t.client.get('/api/light/intake',params={'id':queued}).json()['status']!='completed'
    t.embeddings.on_request=None


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_ranked_fragment_survives_parent_expansion_with_a_small_evidence_budget(native_turn, tmp_path):
    t = native_turn
    folder = tmp_path/'source'
    folder.mkdir()
    marker = 'CONTROL_MARKER_1735'
    paragraphs = [('Unrelated paragraph '+str(i)+'. ')*25 for i in range(3)]
    paragraphs.append(marker+' '+('The matching detail stays with its source. '*14))
    (folder/'sections.txt').write_text('\n\n'.join(paragraphs),encoding='utf-8')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Parent budget'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    read=source_read(t,dataset,'sections.txt');read['args'].update(q=marker,max_chars=900)
    t.scripts.extend([{'tool':'search_sources','args':{'q':marker,'limit':1,'max_chars':900}},read,
                      {'text':'Confirmed source [Источник 1]'}])
    response=t.client.post('/api/chat',json={'question':'Read the exact control marker','mode':'search',
        'retrieval_mode':'pull',
        'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':True}}).raise_for_status().json()
    assert marker in json.dumps(t.seen[-1]['messages'])
    assert response['source_map'][0]['doc_name'].endswith('sections.txt')


@pytest.mark.parametrize('native_turn', ['rag'], indirect=True)
def test_push_pull_and_hybrid_share_retrieval_and_keep_plain_chat_unscoped(native_turn, tmp_path):
    t = native_turn
    proof = 'Номинальная мощность контрольной установки составляет 17,35 кВт.'
    folder = tmp_path / 'Источники с пробелами'
    folder.mkdir()
    (folder/'Параметры.txt').write_text(proof, encoding='utf-8')
    (folder/'Архив.txt').write_text('Архивная запись о резервном отделе и квартальном обзоре проходов.', encoding='utf-8')
    dataset = t.client.post('/api/rag/datasets', json={'name':'Нормативы'}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external', json={'path':str(folder),'dataset_id':dataset,
                       'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda: t.client.get('/api/light/intake', params={'id':job}).json().get('status') == 'completed', timeout=40)

    def tools(supported):
        stamp = datetime.now(timezone.utc)
        names = [CapabilityName.CHAT_COMPLETIONS, CapabilityName.STREAMING]
        if supported:
            names.append(CapabilityName.TOOLS)
        t.registry.save_capability_snapshot(CapabilitySnapshot(
            f'cap:mode-{supported}', t.revision.revision_id,
            tuple(CapabilityObservation(name,CapabilityState.SUPPORTED,'probe',stamp) for name in names),
            stamp, datetime(2099,1,1,tzinfo=timezone.utc), {'observed_context_tokens':'24000'},
        ), actor='admin:test')

    question = 'Какая мощность у контрольной установки?'
    scope = {'scope_type':'dataset','dataset_ids':[dataset],'project_ids':[],'selected_sources_only':True}
    base = {'question':question,'scope':scope,'mode':'search'}
    answer = source_excerpt_reply

    def ask(mode, script, **overrides):
        t.seen.clear()
        t.scripts.extend(script)
        return t.client.post('/api/chat', json={**base,'retrieval_mode':mode,**overrides})

    def delivered(body):
        text=json.dumps(body['messages'], ensure_ascii=False)
        return proof in text and '[Фрагмент S' in text

    def check_result(result, mode):
        assert result['retrieval_trace']['mode'] == mode
        assert '17.35' in result['answer'].replace(',', '.')
        assert result['source_map'][0]['dataset_id'] == dataset
        assert result['source_map'][0]['doc_name'].endswith('Параметры.txt')
        assert result['source_map'][0]['url'].startswith('/api/')
        return result['source_map'][0]['qdrant_point_id']

    tools(False)
    before = len(t.embeddings.calls)
    pushed = ask('push', [answer]).raise_for_status().json()
    assert len(t.seen) == 1 and delivered(t.seen[0])
    assert not t.seen[0].get('tools')
    assert pushed['retrieval_trace']['prefetch']['candidates'] >= 1
    assert pushed['artifact']['tool_results'] == []
    point = check_result(pushed, 'push')
    assert len(t.embeddings.calls) == before + 1

    tools(True)
    call = {'tool':'search_sources','args':{'q':question}}
    read=source_read(t,dataset,'Параметры.txt')
    pulled = ask('pull', [call, read, answer]).raise_for_status().json()
    assert not delivered(t.seen[0]) and delivered(t.seen[-1])
    assert delivered(t.seen[1])
    assert pulled['retrieval_trace']['prefetch'] is None
    assert check_result(pulled, 'pull') == point

    hybrid = ask('hybrid', [call, read, answer]).raise_for_status().json()
    assert all(delivered(b) for b in t.seen)
    assert any(tool['function']['name']=='search_sources' for tool in t.seen[0]['tools'])
    assert hybrid['artifact']['tool_results'][0]['tool'] == 'search_sources'
    assert check_result(hybrid, 'hybrid') == point

    before = len(t.embeddings.calls)
    plain = ask('push', [{'text':'Обычный разговор'}], scope={'scope_type':'none'}).raise_for_status().json()
    assert plain['retrieval_trace']['mode'] == 'none' and plain['source_map'] == []
    assert not delivered(t.seen[0])
    assert len(t.embeddings.calls) == before
    invalid = ask('typo', [])
    assert invalid.status_code == 422 and invalid.json()['code'] == 'CHAT_RETRIEVAL_MODE_INVALID'
    assert t.seen == []

    if model := os.getenv('LES_TEST_LOCAL_RAG_MODEL'):
        revision = t.registry.create_connection(
            display_name='Local acceptance model', base_url='http://127.0.0.1:11434',
            model_id=model, locality=ConnectionLocality.LOOPBACK, requested_context_tokens=8192,
            secret_ref=None, extension_type=None, actor='admin:test',
        )
        stamp = datetime.now(timezone.utc)
        t.registry.save_capability_snapshot(CapabilitySnapshot(
            'cap:local-rag', revision.revision_id,
            (CapabilityObservation(CapabilityName.CHAT_COMPLETIONS,CapabilityState.SUPPORTED,'probe',stamp),),
            stamp, datetime(2099,1,1,tzinfo=timezone.utc),
            {'observed_context_tokens':'8192','chat_protocol':'native_chat_v1'},
        ), actor='admin:test')
        binding = t.registry.get_role_binding(ConnectionRole.ANSWER)
        t.registry.bind_role(ConnectionRole.ANSWER, revision.revision_id,
                             expected_binding_revision=binding.binding_revision, actor='admin:test')
        result = t.client.post('/api/chat', json={**base,'retrieval_mode':'push',
                               'question':'Какова номинальная мощность контрольной установки? Назови число и единицу.',
                               'response_length':'short'}, timeout=150).raise_for_status().json()
        assert check_result(result, 'push') == point
