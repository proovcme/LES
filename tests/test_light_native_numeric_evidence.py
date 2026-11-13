"""Spreadsheet records use the same native numeric commit as physical PDF cells."""
import json
import re

import pytest
from openpyxl import Workbook
from tests.test_light_native_api import wait_for
from tests.test_light_native_chat_turn import native_turn
from tests.pdf_fixtures import write_pdf


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
@pytest.mark.parametrize('kind',['spreadsheet','prose'])
@pytest.mark.parametrize('mode',['search','agent'])
@pytest.mark.parametrize('retrieval_mode',['push','hybrid'])
def test_numeric_record_cannot_bypass_binding_by_using_field_text_or_prose(native_turn,tmp_path,kind,mode,retrieval_mode):
    t=native_turn;folder=tmp_path/'documents';folder.mkdir()
    if kind=='spreadsheet':
        book=Workbook();sheet=book.active;sheet.title='Data'
        sheet.append(['Position','Name','Unit','Quantity'])
        sheet.append(['A','Alpha fixture','m',17.25])
        book.save(folder/'schedule.xlsx');book.close()
    else:write_pdf(folder/'schedule.pdf',['Alpha fixture quantity: 17.25 m. Ordinary prose without a grid.'])
    dataset=t.client.post('/api/rag/datasets',json={'name':'Spreadsheet records'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    wait_for(lambda:t.client.get('/api/light/intake',params={'id':job}).json().get('status')=='completed',timeout=40)
    def select_record(body):
        text='\n'.join(m['content'] for m in body['messages'])
        assert 'Alpha fixture: 999 m' not in text, 'Rejected numeric draft was fed back as evidence'
        assert 'TABLE_FACTS_REQUIRED' in text
        fragments=re.split(r'\[Фрагмент (S\d+)\]',text)
        chosen=next(fragments[i] for i in range(1,len(fragments),2) if 'Alpha fixture' in fragments[i+1])
        return {'text':json.dumps({'answerable':True,'rows':[chosen],'sums':[]})}
    t.scripts.extend([{'text':'Alpha fixture: 999 m [Источник 1]'},select_record])
    result=t.client.post('/api/chat',json={'question':'How much Alpha fixture is specified?',
        'mode':mode,'retrieval_mode':retrieval_mode,'scope':{'scope_type':'dataset','dataset_ids':[dataset],'selected_sources_only':False}}).raise_for_status().json()
    assert result['artifact']['validation']=='source_rows_verified'
    proof=result['artifact']['table_facts']['rows'][0]
    assert proof['kind']=='source_excerpt' and '17.25' in proof['cells'][0]
    assert 'Alpha fixture' in result['answer'] and '999' not in result['answer']
    source=next(s for s in result['source_map'] if s['index']==proof['source_index'])
    assert source['quote']==proof['cells'][0]
    if kind=='spreadsheet':assert source['sheet_name']=='Data' and source['row']==2
    else:assert source['page']==1
    assert len(t.seen)==2
