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


def grid(path,transform=False,thick=False,quantity='17.25',continuation=False):
    xs=[20,70,190,240,290];ys=[160,190,220,250,280]
    cells=[['Position','Item','Unit','Quantity'],['1','Alpha item','pcs',quantity],['2','Beta item','m','23.50']]
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
            assert schema['required']==['answerable','rows','sums'] and schema['properties']['rows']['items']['type']=='string'
        return {'text':json.dumps({'answerable':True,'rows':[f'S1R{row}'],'sums':[]})}
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
    with closing(QdrantClient(url=t.env['LES_LIGHT_QDRANT_URL'],api_key=t.env['LES_LIGHT_QDRANT_API_KEY'],check_compatibility=False)) as c:
        c.set_payload(collection,payload={'pdf_page_view_sha256':'0'*64},points=[views[0].id],wait=True)
    failed=t.client.post('/api/chat',json={'question':'How much Alpha item is in the documents?',
        'mode':mode,'retrieval_mode':'push','scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':strict}})
    assert failed.status_code==503 and failed.json()['code']=='PDF_PAGE_VIEW_INVALID'
    assert len(t.seen)==3
