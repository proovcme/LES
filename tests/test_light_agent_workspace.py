"""Real Rust memory/files plus session-bound model tools and independent undo."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
from pathlib import Path
import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from proxy.services import agent_state_service as state, chat_session_service as sessions, project_service as projects
from proxy.services import agent_workspace_tools as tools

@pytest.fixture
def root(tmp_path,monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build the native agent-files owner')
    root=tmp_path/'Данные Леса 日本語';(root/'data').mkdir(parents=True)
    db=root/'data/meta.db'
    monkeypatch.setenv('LES_PRODUCT_EDITION','light');monkeypatch.setenv('LES_STATE_ROOT',str(root))
    for module in (state,sessions,projects):monkeypatch.setattr(module,'rag_meta_db_path',lambda:str(db))
    return root

def test_user_global_project_memory_scope_and_plain_file_edit(root):
    one=projects.create_project('One')['id'];two=projects.create_project('Two')['id']
    state.call('save',area='user',expected_revision='missing',text='Говори по-русски 🌲')
    state.call('save',area='memory',expected_revision='missing',text='Общее')
    state.call('save',project_id=one,area='memory',expected_revision='missing',text='Только One')
    assert [d['text'] for d in state.call('bundle',project_id=one)['documents']]==['Говори по-русски 🌲','Общее','Только One']
    assert [d['text'] for d in state.call('bundle',project_id=two)['documents']]==['Говори по-русски 🌲','Общее','']
    path=root/'agent/USER.md';path.write_text('Внешняя правка 日本語',encoding='utf-8-sig')
    read=state.call('read',area='user');assert read['text']=='Внешняя правка 日本語'
    changed=state.call('save',area='user',expected_revision=read['revision'],text='New')
    state.call('restore',area='user',expected_revision=changed['revision'],history_id=changed['history_id'],side='before')
    assert path.read_bytes().startswith(b'\xef\xbb\xbf')

def test_concurrent_writers_keep_one_commit_and_original_sources(root,tmp_path):
    source=tmp_path/'Source.txt';source.write_text('Source stays read-only',encoding='utf-8');before=source.read_bytes()
    def write(index):
        try:return state.call('save',area='files',path='notes/План #1.md',expected_revision='missing',text=f'writer {index}')['persisted']
        except state.AgentStateError as error:return error.code
    with ThreadPoolExecutor(max_workers=16) as pool:results=list(pool.map(write,range(16)))
    assert results.count(True)==1 and results.count('AGENT_CONFLICT')==15
    assert len(state.call('history',area='files',path='notes/План #1.md')['items'])==1
    for path in ['../Source.txt',str(source),'a:secret','CON.txt']:
        with pytest.raises(state.AgentStateError):state.call('save',area='files',path=path,expected_revision='missing',text='overwrite')
    assert source.read_bytes()==before

def test_permission_and_scope_are_not_model_arguments(root):
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    one=projects.create_project('One')['id'];two=projects.create_project('Two')['id']
    sid=sessions.create_session(project_id=one)['session_id']
    harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sid)
    assert tools.enabled(sid,'agent') and not tools.enabled(sid,'search')
    result=harness.call('agent_workspace',{'operation':'save','area':'memory','text':'One only','expected_revision':'missing'})
    assert result['status']=='ok' and result['result']['persisted']
    assert state.call('read',area='memory',project_id=one)['text']=='One only'
    assert not state.call('read',area='memory',project_id=two)['exists']
    forbidden=harness.call('agent_workspace',{'operation':'save','area':'memory','text':'leak','expected_revision':'missing','project_id':two})
    assert forbidden['status']=='error'
    config=state.call('policy')['revision']
    state.call('policy_save',area='policy',expected_revision=config,value={'auto_memory':False,'workspace_write':False})
    blocked=harness.call('agent_workspace',{'operation':'save','area':'user','text':'blocked','expected_revision':'missing'})
    assert blocked['status']=='error'
    assert harness.call('agent_workspace',{'operation':'read','area':'memory'})['status']=='ok'
    assert not (root/'agent/USER.md').exists()

def test_tool_edits_a_large_file_without_rewriting_it(root):
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    sid=sessions.create_session()['session_id'];harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sid)
    text='Начало\n'+('row\n'*3000)+'TARGET 日本語\nКонец'
    saved=state.call('save',area='files',path='data/rows.csv',expected_revision='missing',text=text)
    page=harness.call('agent_workspace',{'operation':'read','area':'files','path':'data/rows.csv'})['result']
    assert page['next_offset']==5000 and page['total_chars']==len(text)
    result=harness.call('agent_workspace',{'operation':'edit','area':'files','path':'data/rows.csv','expected_revision':saved['revision'],'old_text':'TARGET 日本語','new_text':'CHANGED 🌲'})
    assert result['status']=='ok'
    assert state.call('read',area='files',path='data/rows.csv')['text']==text.replace('TARGET 日本語','CHANGED 🌲')

def test_agent_workspace_does_not_displace_scoped_rag_tools(root):
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    sid=sessions.create_session()['session_id'];harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sid)
    selected=['web_search','web_read','filesystem_list','filesystem_stat','filesystem_read_text','agent_workspace','search_sources','read_source']
    result=harness.shortlist('Работать с документами',mode='agent',allowed_tools=selected,dataset_ids=('explicit-dataset',),
        runtime_available=frozenset(selected),limit=5)
    names=[item['name'] for item in result['tools']]
    assert {'agent_workspace','search_sources','read_source'}<=set(names)

def test_memory_projection_is_bounded_advisory_and_records_omissions(root):
    from proxy.services.context_governor_service import ContextKind
    from proxy.services.typed_memory_projection_service import project_memory,MemoryLimits
    from proxy.services.chat_evidence_context import workspace_memory_objects
    sid=sessions.create_session()['session_id']
    state.call('save',area='user',expected_revision='missing',text='Короткий ответ\n\n'+'X'*1600)
    from proxy.services import memory_service,conversation_context_service
    # Their existing stores share the fixture DB for a whole-context test.
    original=[memory_service.rag_meta_db_path,conversation_context_service.rag_meta_db_path]
    memory_service.rag_meta_db_path=state.rag_meta_db_path;conversation_context_service.rag_meta_db_path=state.rag_meta_db_path
    try:projection=project_memory(session_id=sid,project_id=None,dataset_ids=(),limits=MemoryLimits())
    finally:memory_service.rag_meta_db_path,conversation_context_service.rag_meta_db_path=original
    objects=workspace_memory_objects(projection.as_context_candidates(),registered=True)
    assert any(obj.object_id.startswith('agent-file:USER.md') for obj in objects)
    assert any(obj.object_id.startswith('agent-memory-omitted:') for obj in objects)
    assert all(item.is_evidence is False for item in projection.items)
    assert not any(candidate.kind is ContextKind.EVIDENCE for candidate in projection.as_context_candidates())

def test_python_http_contract_auth_cas_history_and_restore(root):
    from proxy.routers import agent_state
    app=FastAPI();app.include_router(agent_state.router)
    with TestClient(app) as client:
        assert client.get('/api/workspace/agent-state').status_code in (401,403)
        app.dependency_overrides[agent_state.require_user]=lambda:object()
        path='/api/workspace/agent-state'
        body={'action':'save','area':'user','expected_revision':'missing','text':'О вас 日本語'}
        first=client.post(path,json=body);assert first.status_code==200
        assert client.post(path,json=body).status_code==409
        assert client.get(path,params={'action':'history','area':'user'}).json()['items'][0]['id']==first.json()['history_id']
        assert client.post(path,json={'action':'restore','area':'user','expected_revision':first.json()['revision'],'history_id':first.json()['history_id'],'side':'before'}).status_code==200
        assert not (root/'agent/USER.md').exists()

def test_local_state_effect_without_server_binding_never_executes():
    import asyncio,time
    from proxy.services.trusted_executor_service import TrustedExecutor,ExecutionRequest
    from proxy.services.tool_contract_service import ToolContract,EffectClass,RetryPolicy,IdempotencyPolicy,ResultBudget
    from proxy.services.tool_registry_service import ToolRegistry,ToolRegistration
    calls=[]
    contract=ToolContract(name='unsafe_local',version='1.0.0',title='Test',category='test',summary='Test',
        input_schema={'type':'object'},result_schema='test',effect=EffectClass.LOCAL_STATE,scopes=('owned_workspace',),
        timeout_seconds=1,retry=RetryPolicy.NEVER,idempotency=IdempotencyPolicy.NONE,result_budget=ResultBudget(1000,10),model_owned_fields=(),provenance='test')
    registry=ToolRegistry([ToolRegistration(contract,lambda a:calls.append(a))])
    request=ExecutionRequest('one','unsafe_local',{},(),'test','admin',None,None,time.monotonic()+3)
    result=asyncio.run(TrustedExecutor(registry).execute(request))
    assert result.code=='TOOL_LOCAL_STATE_PERMISSION_REQUIRED' and calls==[]


def test_native_tool_requires_revision_and_preserves_unicode_paging(root):
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    sid=sessions.create_session()['session_id'];harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sid)
    bad=harness.call('agent_workspace',{'operation':'save','area':'user','text':'Коротко'})
    assert bad['status']=='error' and 'expected_revision' in bad['result']['detail']
    assert not (root/'agent/USER.md').exists()
    current=harness.call('agent_workspace',{'operation':'read','area':'user'})['result']
    good=harness.call('agent_workspace',{'operation':'save','area':'user','text':'Коротко','expected_revision':current['revision']})
    assert good['result']['persisted']
    text='🌲日本語'*1500+'Конец'
    state.call('save',area='files',path='unicode.md',text=text,expected_revision='missing')
    page=harness.call('agent_workspace',{'operation':'read','area':'files','path':'unicode.md'})['result']
    assert page['text']==text[:5000] and page['next_offset']==5000
    last=harness.call('agent_workspace',{'operation':'read','area':'files','path':'unicode.md','offset':5000})['result']
    assert last['text']==text[5000:] and last['next_offset'] is None


def test_native_binding_rechecks_project_and_denies_forged_scope(root):
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    import sqlite3
    one=projects.create_project('One')['id'];two=projects.create_project('Two')['id']
    sid=sessions.create_session(project_id=one)['session_id'];harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sid)
    # An independent catalog edit cannot silently retarget an already bound tool.
    with sqlite3.connect(state.rag_meta_db_path()) as db:db.execute('UPDATE les_chat_sessions SET project_id=? WHERE session_id=?',(two,sid))
    result=harness.call('agent_workspace',{'operation':'save','area':'memory','text':'Must not leak','expected_revision':'missing'})
    assert result['result']['code']=='AGENT_SCOPE_CHANGED'
    assert not state.call('read',area='memory',project_id=one)['exists']
    assert not state.call('read',area='memory',project_id=two)['exists']
    with pytest.raises(state.AgentStateError) as error:
        state.call('tool_execute',session_id=sid,bound_project_id=two,tool_arguments={'operation':'read','area':'memory','project_id':one})
    assert error.value.code=='AGENT_INVALID'


def test_delete_can_be_found_after_new_chat_and_export_preserves_bom(root):
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    sid=sessions.create_session()['session_id'];harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sid)
    saved=harness.call('agent_workspace',{'operation':'save','area':'files','path':'notes/план.md','text':'\ufeffПлан 🌲','expected_revision':'missing'})['result']
    exported=state.call('export',area='files',path='notes/план.md',expected_revision=saved['revision'])
    assert exported['text']=='\ufeffПлан 🌲' and exported['filename']=='план.md'
    with pytest.raises(state.AgentStateError):state.call('export',area='files',path='notes/план.md',expected_revision='missing')
    removed=harness.call('agent_workspace',{'operation':'delete','area':'files','path':'notes/план.md','expected_revision':saved['revision']})['result']
    second=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(second,sessions.create_session()['session_id'])
    listed=second.call('agent_workspace',{'operation':'list','area':'files'})['result']['files']
    assert listed[0]['path']=='notes/план.md' and listed[0]['exists'] is False
    restored=second.call('agent_workspace',{'operation':'restore','area':'files','path':'notes/план.md','expected_revision':'missing','history_id':removed['history_id'],'side':'before'})
    assert restored['result']['revision']==saved['revision']


def test_current_request_separates_real_receipts_from_past_model_claims(root):
    from proxy.services.execution_receipt_service import no_local_changes, execution_incomplete, local_file_receipt
    envelope=tools.current_request('Измени файл',[])
    assert envelope['local_execution_current_request']['effects']==[]
    assert envelope['local_execution_current_request']['previous_messages_are_not_receipts'] is True
    assert not execution_incomplete({'execution':no_local_changes()})
    receipt=local_file_receipt({'status':'ok','operation':'delete','result':{'persisted':True,'changed':True,'revision':'missing','path':'workspaces/0/one.md','history_id':'one'}})
    assert receipt['operation']=='delete' and receipt['changed'] and 'историю' in receipt['notice']
    proof=tools.current_request('Измени файл',[receipt])['local_execution_current_request']['effects']
    assert proof[0]['history_id']=='one' and proof[0]['path']=='workspaces/0/one.md'
