"""Automatic Word scope through native MCP, without a generator or file filter."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

import pytest
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


@pytest.fixture(autouse=True)
def section_reading(monkeypatch):
    monkeypatch.setenv('LES_LIGHT_INSTANCE_ID', 'word-window-fixture')
    parsers=os.getenv('LES_TEST_NATIVE_DOCUMENT_PARSERS')
    if not parsers:pytest.skip('Native Docling parser required for canonical table checks')
    monkeypatch.setenv('LES_NATIVE_DOCUMENT_PARSERS',parsers)
    monkeypatch.setenv('LES_EXPERIMENT_WORD_READING', 'section-v1')
    monkeypatch.setenv('LES_EXPERIMENT_SAVED_READING', 'original-v1')
    monkeypatch.setenv('LES_EXPERIMENT_EVIDENCE_PACKET', 'compact-v1')
    monkeypatch.setenv('LES_EXPERIMENT_OVERVIEW', 'documents-v2')
    monkeypatch.setenv('LES_EXPERIMENT_OVERVIEW_ADMISSION', 'ready-v1')


def mcp(t, state, tool, args):
    runtime = os.environ.get('LES_TEST_NATIVE_RUNTIME')
    if not runtime:
        pytest.skip('Native runtime required for actual MCP transport')
    state.mkdir(exist_ok=True)
    instance = t.client.get('/api/light/instance').raise_for_status().json()['instance_id']
    (state/'launcher-status.json').write_text(json.dumps({'schema':'les.light-launcher.v1',
        'phase':'ready','instance_id':instance,'api_url':str(t.client.base_url).rstrip('/')}),encoding='utf-8')
    frames=[{'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2025-11-25',
        'capabilities':{},'clientInfo':{'name':'word-window-test','version':'1'}}},
        {'jsonrpc':'2.0','method':'notifications/initialized'},
        {'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':tool,'arguments':args}}]
    result=subprocess.run([runtime,'--cli','mcp'],input='\n'.join(json.dumps(f) for f in frames)+'\n',
        encoding='utf-8',capture_output=True,timeout=90,
        env=os.environ|{'LES_LIGHT_STATE':str(state),'LES_LIGHT_MCP_API_KEY':''},
        creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    assert result.returncode==0,result.stderr
    return next(v for line in result.stdout.splitlines() if (v:=json.loads(line)).get('id')==2)


@pytest.mark.parametrize('native_turn',['rag-small'],indirect=True)
def test_word_window_mcp_keeps_complete_rows_original_positions_and_revision_guards(native_turn,tmp_path):
    from docx import Document
    t=native_turn;folder=tmp_path/'originals';folder.mkdir();path=folder/'Sections.docx'
    doc=Document()
    for section in range(3):
        doc.add_heading(f'Section {section}',level=1)
        for n in range(75):
            doc.add_paragraph(f'Paragraph {section}-{n}. '+('Ordinary unrelated explanatory text. '*8))
        if section==1:
            doc.add_heading('WINDOW_TARGET equipment ratings',level=2)
            doc.add_paragraph('The table gives separate installed ratings; do not combine its rows.')
            table=doc.add_table(rows=1,cols=3)
            for cell,text in zip(table.rows[0].cells,['Item','Capacity','Unit']):cell.text=text
            for name,number in [('WINDOW_TARGET PANEL-Z17','17.25'),('Other panel','117.25')]:
                for cell,text in zip(table.add_row().cells,[name,number,'kW']):cell.text=text
            doc.add_paragraph('Notes: the installed value and operating allowance are different requirements.')
    doc.save(path);digest=hashlib.sha256(path.read_bytes()).hexdigest()
    dataset=t.client.post('/api/rag/datasets',json={'name':'Section scope'}).raise_for_status().json()['id']
    job=t.client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':dataset,'background':True}).raise_for_status().json()['intake_job_id']
    def indexed():
        value=t.client.get('/api/light/intake',params={'id':job}).raise_for_status().json()
        assert value.get('status') not in ('failed','cancelled'),value
        return value.get('status')=='completed'
    wait_for(indexed,timeout=90)
    state=tmp_path/'mcp';reply=mcp(t,state,'search_sources',{'query':'WINDOW_TARGET PANEL-Z17 capacity',
        'dataset_ids':[dataset],'view':'overview','max_chars':12000})
    assert not reply.get('error') and not reply['result'].get('isError'),reply
    envelope=reply['result']['structuredContent'];value=envelope['result'];plan=value['read_plan']['word']
    assert plan['mode']=='section-v1' and plan['matched_units']>0
    assert plan['retained_units']<plan['original_units']/2 and plan['omitted_units']>0
    assert value['coverage']=='bounded' and value['next_offset'] is None
    passages=value['passages'];sources=envelope['sources']
    table=next((p for p in passages if (p.get('canonical') or {}).get('kind')=='table'
        and 'PANEL-Z17' in p['text']),None)
    assert table is not None,{'plan':plan,'read_failures':value.get('read_failures'),
        'passages':[{'kind':(p.get('canonical') or {}).get('kind'),'text':p['text'][:350]} for p in passages]}
    assert all(text in table['text'] for text in ['Capacity','17.25','117.25','kW'])
    source=sources[table['source']['ref']-1]
    assert source['source_sha256']==digest and source['ordinal']>75 and source['read_offset']>75
    assert source['evidence_kind']=='physical_word_section'
    direct=mcp(t,state,'read_source',{'doc_id':source['doc_id'],'source_sha256':digest,
        'view_sha256':source['view_sha256'],'offset':source['read_offset'],'limit':1,'max_chars':12000})
    assert not direct.get('error') and not direct['result'].get('isError'),direct
    reread=direct['result']['structuredContent']['result']['passages'][0]
    assert reread['text']==table['text'] and reread['canonical']==table['canonical']
    assert reread['source']['ordinal']==source['ordinal']
    original=t.client.get(source['url'].split('#')[0]).raise_for_status().content
    assert hashlib.sha256(original).hexdigest()==digest and not t.seen
    path.write_bytes(path.read_bytes()+b'changed fixture revision')
    rejected=mcp(t,state,'read_source',{'doc_id':source['doc_id'],'source_sha256':digest,
        'view_sha256':source['view_sha256'],'offset':source['read_offset'],'limit':1})
    assert rejected.get('error') or rejected['result'].get('isError'),rejected
    assert 'REVISION_CHANGED' in json.dumps(rejected)
