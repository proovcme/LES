"""Physical cells must survive content-stream order, transforms and page reads."""
import hashlib
from datetime import datetime, timezone
import json
import os
import re
from pathlib import Path
import subprocess

import pytest
from tests.pdf_fixtures import write_pdf
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


def grid(path,transform=False,thick=False,quantity='17.25',continuation=False,unit='pcs'):
    xs=[20,70,190,240,290];ys=[160,190,220,250,280]
    cells=[['Position','Item','Unit','Quantity'],['1','Alpha item',unit,quantity],['2','Beta item','m','23.50']]
    ops=['q 0.8 0 0 0.8 15 20 cm'] if transform else []
    ops += [f'{x} 160 m {x} 280 l S' for x in xs]
    ops += [f'20 {y} m 290 {y} l S' for y in ys]
    if thick:ops += [f'{x} 160 1.2 120 re f' for x in xs]
    # Content order is deliberately column-major, not visual row-major.
    for col in range(4):
        for row,values in enumerate(cells):
            ops.append(f'BT /F1 8 Tf {xs[col]+3} {266-row*30} Td ({values[col]}) Tj ET')
    if continuation:ops.append('BT /F1 8 Tf 73 226 Td (continued description) Tj ET')
    if transform:ops.append('Q')
    return write_pdf(path,['\n'.join(ops).encode('ascii')])


def extract(path,**options):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native PDF reader required')
    import pypdfium2_raw
    library=Path(pypdfium2_raw.__file__).with_name('pdfium.dll')
    env={**os.environ,'PYTHONHOME':str(path.parent/'no-python'),'PYTHONPATH':str(path.parent/'no-code')}
    p=subprocess.run([program,'--serve-pdf-text',str(library)],
        input=json.dumps({'request_id':1,'input':{'path':str(path),**options}})+'\n',
        capture_output=True,text=True,encoding='utf-8',timeout=20,env=env)
    assert p.returncode==0,p.stderr
    value=json.loads(p.stdout)['result'];assert value['status']=='completed',value
    return value['value']


@pytest.mark.parametrize('transform',[False,True])
def test_visual_grid_keeps_description_unit_and_decimal_in_the_same_row(tmp_path,transform):
    path=grid(tmp_path/'schedule.pdf',transform);before=hashlib.sha256(path.read_bytes()).hexdigest()
    result=extract(path)
    text=result['page_views'][0]['text']
    assert '|Position|Item|Unit|Quantity|' not in result['text']
    assert '|Position|Item|Unit|Quantity|' in text
    assert '|1|Alpha item|pcs|17.25|' in text
    assert '|2|Beta item|m|23.50|' in text
    assert hashlib.sha256(path.read_bytes()).hexdigest()==before


def test_ordinary_prose_without_a_confirmed_grid_keeps_its_existing_text(tmp_path):
    path=write_pdf(tmp_path/'prose.pdf',['Ordinary prose 17.25 and unit m; no inferred table.'])
    assert 'Ordinary prose 17.25 and unit m; no inferred table.' in extract(path)['text']


@pytest.mark.parametrize('quantity',['-17.25','0','1000.05',''])
def test_thick_rules_and_wrapped_description_do_not_create_columns_or_change_values(tmp_path,quantity):
    path=grid(tmp_path/'schedule.pdf',thick=True,quantity=quantity,continuation=True)
    text=extract(path)['page_views'][0]['text']
    assert '|Position|Item|Unit|Quantity|' in text
    assert f'|1|Alpha item continued description|pcs|{quantity}|' in text
    assert text.count('Alpha item')==1 and '|2|Beta item|m|23.50|' in text


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
@pytest.mark.parametrize('strict',[False,True])
@pytest.mark.parametrize('mode',['search','agent'])
def test_chat_reads_cells_while_the_index_keeps_its_search_representation(native_turn,tmp_path,strict,mode):
    from contextlib import closing
    from qdrant_client import QdrantClient
    t=native_turn;folder=tmp_path/'documents';folder.mkdir();grid(folder/'schedule.pdf')
    if strict:
        from proxy.services.model_connection_contracts import CapabilityName, CapabilityObservation, CapabilitySnapshot, CapabilityState
        observed=datetime(2026,1,1,tzinfo=timezone.utc)
        t.registry.save_capability_snapshot(CapabilitySnapshot('cap:table-schema',t.revision.revision_id,
            tuple(CapabilityObservation(cap,CapabilityState.SUPPORTED,'probe',observed) for cap in
                (CapabilityName.CHAT_COMPLETIONS,CapabilityName.STREAMING,CapabilityName.TOOLS,CapabilityName.STRUCTURED_OUTPUT)),
            observed,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'8192'}),actor='admin:test')
    dataset=t.client.post('/api/rag/datasets',json={'name':'Two document views'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'],api_key=t.env['LES_LIGHT_QDRANT_API_KEY'],check_compatibility=False)) as c:
        collection=c.get_collections().collections[0].name
        points,_=c.scroll(collection,limit=100,with_payload=True)
    views=[p for p in points if p.payload.get('pdf_page_view')]
    assert len(views)==1 and '|1|Alpha item|pcs|17.25|' in views[0].payload['pdf_page_view']
    assert '|1|Alpha item|pcs|17.25|' not in views[0].payload['text']
    def answer(body):
        text='\n'.join(m['content'] for m in body['messages'])
        assert 'C1: 1; C2: Alpha item; C3: pcs; C4: 17.25' in text
        row=int(re.search(r'R(\d+) C1: 1; C2: Alpha item',text)[1])
        assert bool(body.get('response_format'))==strict
        if strict:
            schema=body['response_format']['json_schema']['schema']
            assert set(schema['required'])=={'answerable','rows','sums','missing'}
            assert schema['properties']['rows']['items']['type']=='string'
        return {'text':json.dumps({'answerable':True,'rows':[f'S1R{row}'],'sums':[],'missing':[]})}
    t.scripts.append(answer)
    result=t.client.post('/api/chat',json={'question':'How much Alpha item is in the documents?','session_id':'numeric-layout',
        'mode':mode,'retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':strict}}).raise_for_status().json()
    assert len(result['source_map'])==1 and result['source_map'][0]['reading_representation']=='pdf_cells'
    assert str(views[0].id) in result['retrieval_trace']['prefetch']['read_receipt']['point_ids']
    assert '|1 · Alpha item · pcs · 17.25|[Источник 1]|' in result['answer']
    assert result['artifact']['validation']=='source_rows_verified'
    assert result['artifact']['table_facts']['rows'][0]['cells']==['1','Alpha item','pcs','17.25']
    assert result['source_map'][0]['quote']=='|1|Alpha item|pcs|17.25|'
    history=t.client.get('/api/chat/history',params={'session_id':'numeric-layout'}).raise_for_status().json()
    assert '|1|Alpha item|pcs|17.25|' in json.dumps(history,ensure_ascii=False)
    # A model's invented numeric prose must not escape in the stream, even before retry.
    t.scripts.extend([{'text':'Alpha item: 999 pcs [Источник 1]'},answer])
    stream=t.client.post('/api/chat/stream',json={'question':'How much Alpha item is in the documents?',
        'mode':mode,'retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':strict}}).raise_for_status().text
    events=[(part.split('\n',1)[0],json.loads(part.split('data: ',1)[1]))
        for part in stream.split('\n\n') if part.startswith('event: ') and 'data: ' in part]
    tokens=''.join(value for event,value in events if event=='event: token')
    assert '999' not in tokens and '17.25' in tokens and '"rows"' not in tokens
    assert any(event=='event: final' and value['artifact']['validation']=='source_rows_verified' for event,value in events)
    expected_calls = 3
    if strict and mode == 'search':
        request = {'question':'How much Alpha item is in the documents?', 'mode':mode,
                   'retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset]}}
        t.scripts.extend([{'text':'Alpha item: 999 pcs [Источник 1]'}] * 2)
        rejected = t.client.post('/api/chat',json=request)
        assert rejected.status_code == 503 and rejected.json()['code'] == 'TABLE_FACTS_REQUIRED'
        t.scripts.append({'text':json.dumps({'answerable':False,'rows':[],'sums':[],'missing':['guarantee']})})
        missing = t.client.post('/api/chat',json={**request,'question':'What is the guarantee?'})
        assert missing.raise_for_status().json()['artifact']['validation'] == 'insufficient_evidence'
        expected_calls += 3
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'],api_key=t.env['LES_LIGHT_QDRANT_API_KEY'],check_compatibility=False)) as c:
        c.set_payload(collection,payload={'pdf_page_view_sha256':'0'*64},points=[views[0].id],wait=True)
    failed=t.client.post('/api/chat',json={'question':'How much Alpha item is in the documents?',
        'mode':mode,'retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':strict}})
    assert failed.status_code==503 and failed.json()['code']=='PDF_PAGE_VIEW_INVALID'
    assert len(t.seen)==expected_calls


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_folder_hybrid_binds_pdf_row_when_prose_is_also_present(native_turn,tmp_path):
    t = native_turn
    folder = tmp_path / 'documents'
    folder.mkdir()
    grid(folder/'Register.pdf')
    (folder/'Notes.txt').write_text('Alpha item is checked at the receiving desk.', encoding='utf-8')
    dataset = t.client.post('/api/rag/datasets',json={'name':'Mixed folder'}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,
                        'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json()['status']=='completed',timeout=40)
    session = 'mixed-register'
    def row_address(body, *, require_address):
        import sqlite3
        with sqlite3.connect(t.database) as db:
            trace, = db.execute('SELECT retrieval_trace_json FROM chat_history WHERE session_id=? ORDER BY id DESC LIMIT 1',(session,)).fetchone()
        sources = json.loads(trace)['source_map']
        digest = hashlib.sha256(b'|1|Alpha item|pcs|17.25|').hexdigest()
        index = next(s['index'] for s in sources if s.get('verified_cells') and s.get('reading_unit_sha256')==digest)
        message = '\n'.join(m['content'] for m in body['messages'])
        line = re.search(r'R(\d+) C1: 1; C2: Alpha item',message)
        if require_address or line:
            assert line, message
            return index, int(line[1])
        assert '|1|Alpha item|pcs|17.25|' in message
        return index, None
    def wrong(body):
        index, _ = row_address(body, require_address=False)
        return {'text':f'Alpha item: 999 pcs [Источник {index}]'}
    def valid(body):
        index, _ = row_address(body, require_address=False)
        instruction = body['messages'][0]['content']
        assert f'{{"source":{index}}}' in instruction and 'sums' not in instruction
        return {'text':json.dumps({'source':index})}
    t.scripts.extend([wrong,valid])
    reply = t.client.post('/api/chat',json={'question':'How much Alpha item is in the documents?',
        'mode':'search','retrieval_mode':'hybrid','session_id':session,
        'scope':{'scope_type':'datasets','dataset_ids':[dataset],'selected_sources_only':False}})
    assert reply.status_code == 200, reply.text
    data = reply.json()
    assert len(t.seen)==2 and data['retrieval_trace']['cell_selection_retry']
    assert '999' not in data['answer'] and '17.25' in data['answer']
    assert data['artifact']['table_facts']['rows'][0]['cells']==['1','Alpha item','pcs','17.25']
    from tools.inspect_chat_turn import inspect
    trace = inspect(t.database, data['history_id'])
    assert trace['outcome']['validation'] == 'source_rows_verified'
    assert len(trace['calls']) == 2 and all(c['sent_to_model'] for c in trace['calls'])
    assert trace['selected_rows'][0]['source_index'] in trace['citations']['linked']
    assert all(text not in json.dumps(trace) for text in ('Alpha item','Register.pdf','receiving desk'))

    def first_answer(body):
        index, _ = row_address(body, require_address=False)
        return {'text': (
            'Количество Alpha item указано как **17,25** (или 17,25 штук).\n\n'
            '*   **Единица измерения:** pcs (pcs = pieces)\n'
            f'*   **Источник:** [Источник {index} | Register.pdf · стр. 1]'
        )}

    session = 'mixed-register-first-answer'
    t.scripts.append(first_answer)
    request = {'question':'Какое количество Alpha item указано в реестре? Укажи единицу и источник.',
               'mode':'search','retrieval_mode':'hybrid','scope':{'scope_type':'datasets','dataset_ids':[dataset]}}
    first = t.client.post('/api/chat',json={**request,'session_id':session}).raise_for_status().json()
    assert len(t.seen) == 3 and not first['retrieval_trace']['cell_selection_retry']
    assert first['artifact']['table_facts']['selection'] == 'model_citation'
    row, = first['artifact']['table_facts']['rows']
    assert row['cells'] == ['1','Alpha item','pcs','17.25']
    assert (row['quantity_column'], row['unit_column']) == (4, 3)
    linked, = first['retrieval_trace']['citation_check']['linked']
    source = next(s for s in first['source_map'] if s['index'] == linked)
    assert row['source_index'] == linked and row['doc_id'] == source['doc_id']
    assert source['quote'] == '|1|Alpha item|pcs|17.25|' and '17.25' in first['answer']
    original = t.client.get('/api/documents/by-id/'+source['doc_id']+'/raw').raise_for_status().content
    assert original == (folder/'Register.pdf').read_bytes()
    assert row['source_sha256'] == hashlib.sha256(original).hexdigest()

    session = 'mixed-register-citation'
    def cite_row(body):
        index, _ = row_address(body, require_address=False)
        assert 'только ссылку' in body['messages'][0]['content']
        return {'text':f'[Источник {index}]'}
    t.scripts.append(cite_row)
    compact = t.client.post('/api/chat',json={**request,'session_id':session}).raise_for_status().json()
    assert len(t.seen) == 4 and compact['answer'] == f'17.25 pcs [Источник {linked}]'

    session = 'mixed-register-conflict'
    def wrong_unit(body):
        index, _ = row_address(body, require_address=False)
        return {'text':f'Alpha item 17.25 kg [Источник {index}]'}
    t.scripts.extend([wrong_unit,valid])
    corrected = t.client.post('/api/chat',json={**request,'session_id':session}).raise_for_status().json()
    assert len(t.seen) == 6 and corrected['retrieval_trace']['cell_selection_retry']
    assert 'kg' not in corrected['answer'] and corrected['artifact']['table_facts']['rows'][0]['cells'] == row['cells']

    from proxy.services.model_connection_contracts import CapabilityName, CapabilityObservation, CapabilitySnapshot, CapabilityState
    observed = datetime(2026, 1, 1, tzinfo=timezone.utc)
    t.registry.save_capability_snapshot(CapabilitySnapshot('cap:pdf-quantity',t.revision.revision_id,
        tuple(CapabilityObservation(cap,CapabilityState.SUPPORTED,'probe',observed) for cap in
            (CapabilityName.CHAT_COMPLETIONS,CapabilityName.STREAMING,CapabilityName.TOOLS,CapabilityName.STRUCTURED_OUTPUT)),
        observed,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'8192'}),actor='admin:test')
    def choose_item(body):
        index, _ = row_address(body, require_address=False)
        assert not body.get('tools')
        schema = body['response_format']['json_schema']['schema']
        assert schema['required'] == ['item'] and index > 0
        return {'text':json.dumps({'item':'Alpha item'})}
    session = 'mixed-register-structured'
    t.scripts.append(choose_item)
    structured = t.client.post('/api/chat',json={**request,'session_id':session}).raise_for_status().json()
    assert len(t.seen) == 7 and structured['answer'] == compact['answer']
    assert structured['artifact']['table_facts']['selection'] == 'named_pdf_position'

    session = 'mixed-register-additional-fields'
    def read_more(body):
        choose_item(body)
        return {'text':'{"item":null}'}
    def desk(body):
        assert 'receiving desk' in '\n'.join(m['content'] for m in body['messages'])
        assert body.get('response_format',{}).get('json_schema',{}).get('name') != 'les_pdf_position'
        import sqlite3
        with sqlite3.connect(t.database) as db:
            raw, = db.execute('SELECT retrieval_trace_json FROM chat_history WHERE session_id=? ORDER BY id DESC LIMIT 1',(session,)).fetchone()
        note = next(s['index'] for s in json.loads(raw)['source_map'] if s['doc_name'].endswith('Notes.txt'))
        return {'text':f'Позицию проверяют на приёмке [Источник {note}].'}
    t.scripts.extend([read_more,desk])
    more = t.client.post('/api/chat',json={**request,'session_id':session,
        'question':'Количество Alpha item есть в реестре. Где его проверяют согласно заметке?'}).raise_for_status().json()
    assert len(t.seen) == 9 and 'приёмке' in more['answer']

    session = 'mixed-register-revision'
    def revise_before_reply(body):
        response = choose_item(body)
        grid(folder/'Register.pdf', quantity='21')
        return response
    t.scripts.append(revise_before_reply)
    changed = t.client.post('/api/chat',json={**request,'session_id':session})
    assert changed.status_code == 503 and changed.json()['code'] == 'SOURCE_REVISION_CHANGED'
