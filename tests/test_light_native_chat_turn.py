"""Native request -> model/tools -> durable history, with no application Python server."""
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread, Event
from types import SimpleNamespace
import hashlib
import json
import os
import sqlite3
import subprocess

import httpx
import pytest
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_contracts import (ConnectionLocality,ConnectionRole,CapabilityName,CapabilityObservation,CapabilitySnapshot,CapabilityState)
from tests.test_light_native_api import free_port, wait_for, MODEL_BRIDGE_TOKEN
from tests.pdf_fixtures import write_pdf

PATH='/api/light/internal-chat'
HEADERS={'X-Les-Model-Token':MODEL_BRIDGE_TOKEN}

@pytest.fixture
def native_turn(tmp_path):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Build native API and set LES_TEST_NATIVE_API')
    import pypdfium2_raw
    state=tmp_path/'Состояние 日本語';state.mkdir();database=state/'meta.db'
    seen=[];scripts=[];blocked=Event();release=Event()
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            body=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            seen.append(body);script=scripts.pop(0) if scripts else {'text':'Ответ 日本語'}
            if callable(script):script=script(body)
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
            def send(delta,finish=None):
                self.wfile.write(('data: '+json.dumps({'model':'fixture:9b','choices':[{'delta':delta,'finish_reason':finish}]},ensure_ascii=False)+'\n\n').encode());self.wfile.flush()
            try:
                if script.get('tool') or script.get('calls'):
                    calls=script.get('calls') or [script]
                    send({'tool_calls':[{'index':i,'id':f'call-fixture-{i}','type':'function','function':{'name':item['tool'],'arguments':json.dumps(item.get('args',{}),ensure_ascii=False)}} for i,item in enumerate(calls)]},'tool_calls')
                else:send({'content':script.get('text','Ответ 日本語')},None if script.get('broken') or script.get('wait') else script.get('finish','stop'))
                if script.get('wait'):
                    blocked.set();release.wait(timeout=15)
                    if script.get('complete_after_wait'):send({},'stop');self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
                if not script.get('broken') and not script.get('wait'):self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
            except OSError:pass
    model=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=model.serve_forever,daemon=True);thread.start()
    registry=ModelConnectionRegistry(database)
    revision=registry.create_connection(display_name='Fixture',base_url=f'http://127.0.0.1:{model.server_port}/v1',model_id='fixture:9b',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=24000,secret_ref=None,extension_type=None,actor='admin:test')
    registry.bind_role(ConnectionRole.ANSWER,revision.revision_id,expected_binding_revision=None,actor='admin:test')
    observed=datetime(2026,1,1,tzinfo=timezone.utc)
    registry.save_capability_snapshot(CapabilitySnapshot('cap:chat-turn',revision.revision_id,tuple(CapabilityObservation(cap,CapabilityState.SUPPORTED,'probe',observed) for cap in (CapabilityName.CHAT_COMPLETIONS,CapabilityName.STREAMING,CapabilityName.TOOLS)),observed,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'24000'}),actor='admin:test')
    (state/'.env').write_text('TRUSTED_NETWORKS=127.0.0.1/32\nTRUSTED_NETWORK_ROLE=admin\n',encoding='utf-8')
    port=free_port();children=[];log=(state/'api.log').open('wb')
    env={**os.environ,'LES_INTERNAL_MODEL_TOKEN':MODEL_BRIDGE_TOKEN,'LES_ENV_PATH':str(state/'.env'),'LES_STARTUP_BACKGROUND_MUTATIONS':'false',
         'LES_NATIVE_PDFIUM':str(Path(pypdfium2_raw.__file__).with_name('pdfium.dll')),
         'PYTHONHOME':str(tmp_path/'no-python'),'PYTHONPATH':str(tmp_path/'no-application'),'PATH':str(tmp_path/'no-programs')}
    for key in ('LES_LIGHT_QDRANT_URL','LES_LIGHT_QDRANT_API_KEY','LES_INTERNAL_INGEST_TOKEN','LES_NATIVE_EMBEDDING_SPEC'):env.pop(key,None)
    if os.getenv('LES_TEST_SKILL_PYTHON'):env['LES_SKILL_PYTHON']=os.environ['LES_TEST_SKILL_PYTHON']
    client=httpx.Client(base_url=f'http://127.0.0.1:{port}',timeout=20,trust_env=False)
    def start():
        p=subprocess.Popen([program,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{free_port()}','--state',str(state),'--database',str(database)],env=env,cwd=tmp_path,stdout=log,stderr=log,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0));children.append(p)
        wait_for(lambda:client.get('/api/profiles').status_code==200);return p
    try:
        start()
        yield SimpleNamespace(client=client,database=database,state=state,seen=seen,scripts=scripts,children=children,start=start,registry=registry,revision=revision,blocked=blocked,release=release,env=env)
    finally:
        release.set();client.close()
        for child in children:
            if child.poll() is None:child.kill()
            child.wait(timeout=10)
        log.close();model.shutdown();model.server_close();thread.join(timeout=5)

def test_json_and_stream_share_native_turn_and_repeated_key_never_calls_model(native_turn):
    t=native_turn;request={'question':'Привет 日本語','session_id':'fixture-turn'};headers={**HEADERS,'Idempotency-Key':'once'}
    assert t.client.post(PATH,json=request).status_code==404
    response=t.client.post(PATH,json=request,headers=headers).raise_for_status()
    assert response.headers['x-les-handler']=='rust.chat'
    result=response.json();assert result['answer']=='Ответ 日本語';assert len(t.seen)==1
    assert request['question'] not in json.dumps(result['retrieval_trace'],ensure_ascii=False)
    assert t.client.post(PATH,json=request,headers=headers).raise_for_status().json()==result
    assert len(t.seen)==1
    assert t.client.post(PATH,json={**request,'question':'Другой'},headers=headers).status_code==409
    stream=t.client.post(PATH+'/stream',json={**request,'question':'Поток'},headers=HEADERS).raise_for_status().text
    assert 'event: token' in stream and 'event: final' in stream and 'event: error' not in stream
    with sqlite3.connect(t.database) as db:
        rows=db.execute('SELECT answer,crag_status,success FROM chat_history ORDER BY id').fetchall()
    assert rows==[('Ответ 日本語','UNVALIDATED',1)]*2

def test_broken_provider_preserves_exact_cause_and_partial_text_without_final(native_turn):
    t=native_turn;t.scripts.append({'text':'Полученная часть 日本語','broken':True})
    stream=t.client.post(PATH+'/stream',json={'question':'Обрыв','session_id':'broken'},headers=HEADERS).raise_for_status().text
    assert 'event: error' in stream and 'UPSTREAM_STREAM_INTERRUPTED' in stream and 'event: final' not in stream
    with sqlite3.connect(t.database) as db:
        answer,status,success,artifact=db.execute('SELECT answer,crag_status,success,artifact_json FROM chat_history').fetchone()
    assert answer=='Полученная часть 日本語' and status=='FAILED' and success==0
    assert json.loads(artifact)['error']['code']=='UPSTREAM_STREAM_INTERRUPTED'

def test_command_palette_and_both_turn_routes_work_without_model_generation(native_turn):
    t=native_turn
    palette=t.client.get('/api/commands').raise_for_status()
    assert palette.headers['x-les-handler']=='rust.chat-commands'
    assert [row['cmd'] for row in palette.json()['commands']]==['/сводка','/мсп','/проекты','/команды']
    request={'question':'/HELP','session_id':'command-fixture'}
    answer=t.client.post(PATH,json=request,headers=HEADERS).raise_for_status().json()
    assert answer['command']['action']=='help' and answer['crag_status']=='DETERMINISTIC'
    stream=t.client.post(PATH+'/stream',json={'question':'/mcp'},headers=HEADERS).raise_for_status().text
    assert 'event: final' in stream and 'mcp_info' in stream and not t.seen
    with sqlite3.connect(t.database) as db:
        assert db.execute('SELECT crag_status,success FROM chat_history ORDER BY id').fetchall()==[('DETERMINISTIC',1)]*2

def test_truncated_generation_is_saved_but_never_marked_as_a_completed_answer(native_turn):
    t=native_turn;t.scripts.append({'text':'Незавершённая часть','finish':'length'})
    stream=t.client.post(PATH+'/stream',json={'question':'Длинный ответ'},headers=HEADERS).raise_for_status().text
    assert 'MODEL_OUTPUT_LIMIT_REACHED' in stream and 'event: final' not in stream
    with sqlite3.connect(t.database) as db:
        assert db.execute('SELECT answer,crag_status,success FROM chat_history').fetchone()==('Незавершённая часть','FAILED',0)

def source_pdf(t):
    path=write_pdf(t.state/'Оригинал 日本語.pdf',['FIRST PAGE ONLY','Physical second page '+('ABCDE12345 '*120)+'END'])
    digest=hashlib.sha256(path.read_bytes()).hexdigest()
    with sqlite3.connect(t.database) as db:
        db.execute("CREATE TABLE datasets(id TEXT PRIMARY KEY,name TEXT,status TEXT,chunk_count INTEGER,dataset_scope TEXT,module_id TEXT)")
        db.execute("INSERT INTO datasets VALUES ('ds','Fixture','INDEXED',2,'user','')")
        db.execute('CREATE TABLE documents(id TEXT,dataset_id TEXT,file_name TEXT,source_path TEXT,file_hash TEXT)')
        db.execute('INSERT INTO documents VALUES (?,?,?,?,?)',('registered-document','ds','folder/Source.pdf',str(path),digest))
    return path,digest

def test_native_tool_reads_real_page_and_finishes_all_cursors_before_model_answer(native_turn):
    t=native_turn;path,digest=source_pdf(t);before=path.read_bytes()
    t.scripts.extend([{'tool':'read_pdf_source','args':{'doc_id':'registered-document','page':2,'max_chars':24}}, {'text':'Страница прочитана [Источник 1]'}])
    response=t.client.post(PATH,json={'question':'Прочти вторую страницу целиком','dataset_ids':['ds']},headers=HEADERS).raise_for_status().json()
    assert len(t.seen)==2 and 'FIRST PAGE ONLY' not in json.dumps(t.seen,ensure_ascii=False)
    payloads=response['artifact']['tool_results'];assert len(payloads)==2
    assert ''.join(p['result']['text'] for p in payloads)=='Physical second page '+('ABCDE12345 '*120)+'END'
    assert payloads[1]['result']['offset']==24
    assert all(p['result']['doc_id']=='registered-document' and p['result']['source_sha256']==digest for p in payloads)
    assert all(s['page']==2 and s['doc_id']=='registered-document' and '#page=2' in s['url'] for s in response['source_map'])
    assert path.read_bytes()==before and response['success'] is True
    original=t.client.get(response['source_map'][0]['url']).raise_for_status()
    assert original.headers['x-les-handler']=='rust.documents' and original.content==before
    stale=t.client.get('/api/documents/by-id/registered-document/raw',params={'source_sha256':'0'*64})
    assert stale.status_code==409 and stale.json()['code']=='SOURCE_REVISION_CHANGED'

def test_pdf_failure_never_requests_confident_final_and_survives_history_reload(native_turn):
    t=native_turn;_,digest=source_pdf(t)
    t.scripts.extend([{'tool':'read_pdf_source','args':{'doc_id':'registered-document','page':999,'source_sha256':digest}}, {'text':'SHOULD NEVER BE REQUESTED'}])
    response=t.client.post(PATH,json={'question':'Прочти страницу','dataset_ids':['ds'],'session_id':'pdf-error'},headers=HEADERS)
    assert response.status_code==503 and response.json()['code']=='PDF_PAGE_OUT_OF_RANGE'
    assert len(t.seen)==1
    history=t.client.get('/api/chat/history',params={'session_id':'pdf-error'}).raise_for_status().json()
    assert 'PDF_PAGE_OUT_OF_RANGE' in json.dumps(history) and 'SHOULD NEVER' not in json.dumps(history)

def test_hard_process_stop_recovers_partial_text_without_replaying_model(native_turn):
    t=native_turn;t.scripts.append({'text':'До падения 日本語','wait':True})
    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=1) as pool:
        future=pool.submit(t.client.post,PATH+'/stream',json={'question':'Ожидание','session_id':'crash'},headers={**HEADERS,'Idempotency-Key':'crashed'})
        assert t.blocked.wait(timeout=5)
        def checkpointed():
            with sqlite3.connect(t.database) as db:return db.execute("SELECT answer FROM chat_history WHERE session_id='crash'").fetchone()==('До падения 日本語',)
        wait_for(checkpointed)
        t.children[-1].kill();t.children[-1].wait(timeout=5);t.release.set()
        try:future.result(timeout=10)
        except httpx.HTTPError:pass
    t.start()
    replay=t.client.post(PATH,json={'question':'Ожидание','session_id':'crash'},headers={**HEADERS,'Idempotency-Key':'crashed'})
    assert replay.status_code==409 and replay.json()['code']=='CHAT_REQUEST_NOT_REPLAYABLE'
    assert len(t.seen)==1
    with sqlite3.connect(t.database) as db:
        answer,status,artifact=db.execute("SELECT answer,crag_status,artifact_json FROM chat_history WHERE session_id='crash'").fetchone()
    assert (answer,status)==('До падения 日本語','INTERRUPTED') and 'CHAT_INTERRUPTED' in artifact

def test_agent_write_uses_native_revisions_and_retry_never_repeats_the_effect(native_turn):
    t=native_turn
    sid=t.client.post('/api/workspace/sessions',json={'title':'Agent fixture'}).raise_for_status().json()['session_id']
    t.scripts.extend([
        {'tool':'agent_workspace','args':{'operation':'read','area':'files','path':'Результат 日本語.txt'}},
        {'tool':'agent_workspace','args':{'operation':'save','area':'files','path':'Результат 日本語.txt','expected_revision':'missing','text':'Создано Rust 日本語'}},
        {'text':'Файл сохранён.'},
    ])
    request={'question':'Сохрани рабочий файл','session_id':sid,'mode':'agent'};headers={**HEADERS,'Idempotency-Key':'write-once'}
    response=t.client.post(PATH,json=request,headers=headers).raise_for_status().json()
    assert len(t.seen)==3
    results=response['artifact']['tool_results'];assert results[-1]['result']['changed'] is True
    files=list(t.state.rglob('Результат 日本語.txt'));assert len(files)==1 and files[0].read_text(encoding='utf-8')=='Создано Rust 日本語'
    revision=results[-1]['result']['revision']
    assert t.client.post(PATH,json=request,headers=headers).raise_for_status().json()==response
    assert len(t.seen)==3 and revision==results[-1]['result']['revision']

def test_external_python_skill_runs_autonomously_through_the_native_turn(native_turn):
    if not os.getenv('LES_TEST_SKILL_PYTHON'):pytest.skip('Set LES_TEST_SKILL_PYTHON to the isolated skill runtime')
    t=native_turn;library=t.state/'storage/skills';package=library/'external-fixture';package.mkdir(parents=True)
    (package/'SKILL.md').write_text('External compatibility fixture',encoding='utf-8')
    script=package/'calculate.py';script.write_text('import importlib.util\ndef calculate(value):\n    assert importlib.util.find_spec("backend") is None\n    assert importlib.util.find_spec("proxy") is None\n    return {"value":value["n"]*2}\n',encoding='utf-8')
    (library/'approved-compute.json').write_text(json.dumps([{'skill':'external-fixture','path':'calculate.py','entrypoint':'calculate','sha256':hashlib.sha256(script.read_bytes()).hexdigest()}]),encoding='utf-8')
    t.scripts.extend([{'tool':'run_skill_calculation','args':{'skill':'external-fixture','path':'calculate.py','items':[{'id':'one','input':{'n':21}}]}},{'text':'Вычисление завершено.'}])
    result=t.client.post(PATH,json={'question':'Выполни одобренный внешний навык','mode':'agent'},headers=HEADERS).raise_for_status().json()
    tool=result['artifact']['tool_results'][0]
    assert tool['status']=='ok' and tool['result']['succeeded']==1
    assert tool['result']['rows'][0]['result']=={'value':42} and len(t.seen)==2

def test_original_link_supports_unindexed_files_and_exact_legacy_provenance(native_turn):
    t=native_turn;path,digest=source_pdf(t)
    with sqlite3.connect(t.database) as db:db.execute('UPDATE documents SET file_hash=NULL')
    original=t.client.get('/api/documents/by-id/registered-document/raw').raise_for_status()
    assert original.content==path.read_bytes() and original.headers['x-les-handler']=='rust.documents'
    linked=t.client.get('/api/documents/by-id/old-chunk-id/raw',params={'dataset_id':'ds','doc_name':'folder/Source.pdf','source_sha256':digest}).raise_for_status()
    assert linked.content==path.read_bytes()
    assert t.client.get('/api/documents/by-id/old-chunk-id/raw').status_code==404
    part=t.client.get('/api/documents/by-id/registered-document/raw',headers={'Range':'bytes=5-14'}).raise_for_status()
    assert part.status_code==206 and part.content==path.read_bytes()[5:15]
    assert part.headers['content-range']==f'bytes 5-14/{path.stat().st_size}'
    assert t.client.head('/api/documents/by-id/registered-document/raw').raise_for_status().content==b''

def test_explicit_selected_file_binds_its_short_name_but_never_another_file(native_turn):
    t=native_turn;source_pdf(t)
    t.scripts.extend([{'tool':'read_pdf_source','args':{'dataset_id':'ds','doc_name':'Source.pdf','page':2}},{'text':'Прочитано [Источник 1]'}])
    request={'question':'Прочти вторую страницу выбранного файла','dataset_ids':['ds'],'target_file':'folder/Source.pdf','selected_sources_only':True}
    result=t.client.post(PATH,json=request,headers=HEADERS).raise_for_status().json()
    assert result['artifact']['tool_results'][0]['result']['doc_name']=='folder/Source.pdf'
    t.scripts.append({'tool':'read_pdf_source','args':{'doc_id':'registered-document','page':2}})
    blocked=t.client.post(PATH,json={**request,'target_file':'different.pdf'},headers=HEADERS)
    assert blocked.status_code==503 and blocked.json()['code']=='TOOL_SCOPE_VIOLATION'
    assert len(t.seen)==3


def test_rejected_extension_call_exposes_schema_and_can_repair_before_physical_read(native_turn):
    t=native_turn;_,digest=source_pdf(t)
    t.client.post(PATH,json={'question':'Prepare','dataset_ids':['ds']},headers=HEADERS).raise_for_status()
    access=next(tool['function']['name'] for tool in t.seen[-1]['tools'] if tool['function']['name'].startswith('use_extensions_'))
    arguments={'doc_id':'registered-document','page':2,'source_sha256':digest}
    t.scripts.extend([
        {'tool':access,'args':{'operation':'call','tool_name':'read_pdf_source','arguments':arguments}},
        {'tool':access,'args':{'operation':'call','name':'read_pdf_source','arguments':arguments}},
        {'text':'Прочитана страница [Источник 1]'},
    ])
    result=t.client.post(PATH,json={'question':'Read actual page','dataset_ids':['ds']},headers=HEADERS).raise_for_status().json()
    rejected,read=result['artifact']['tool_results']
    assert rejected['status']=='error' and rejected['result']['code']=='TOOL_SCHEMA_INVALID'
    assert rejected['result']['input_schema']['properties']['name']['type']=='string'
    assert 'input_schema' in json.dumps(t.seen[-2],ensure_ascii=False)
    assert read['status']=='ok' and read['result']['tool']=='read_pdf_source'
    assert read['result']['result']['complete'] is True
    assert result['source_map'][0]['page']==2


def test_grounded_pdf_chat_keeps_bound_workspace_reachable_and_rereads_after_save(native_turn):
    t=native_turn;path,digest=source_pdf(t)
    sid=t.client.post('/api/workspace/sessions',json={'title':'Sources and workspace'}).raise_for_status().json()['session_id']
    request={'question':'Сохрани результат в рабочие файлы','dataset_ids':['ds'],'session_id':sid,'mode':'agent'}
    t.client.post(PATH,json={**request,'question':'Prepare'},headers=HEADERS).raise_for_status()
    schemas=t.seen[-1]['tools'];names=[tool['function']['name'] for tool in schemas]
    assert 'read_pdf_source' in names and 'search_sources' in names
    access=next(name for name in names if name.startswith('use_extensions_'))
    read={'operation':'read','area':'files','path':'Result.txt'}
    t.scripts.extend([
        {'tool':access,'args':{'operation':'call','name':'agent_workspace','arguments':read}},
        {'tool':access,'args':{'operation':'call','name':'agent_workspace','arguments':{
            'operation':'save','area':'files','path':'Result.txt','expected_revision':'missing','text':'NATIVE WORKSPACE CONTENT'}}},
        {'tool':access,'args':{'operation':'call','name':'agent_workspace','arguments':read}},
        {'text':'Результат записан и прочитан.'},
    ])
    result=t.client.post(PATH,json=request,headers=HEADERS).raise_for_status().json()
    rows=result['artifact']['tool_results']
    assert len(rows)==3 and all(row['status']=='ok' for row in rows)
    assert 'NATIVE WORKSPACE CONTENT' in json.dumps(rows[-1])
    assert list(t.state.rglob('Result.txt'))[0].read_text(encoding='utf-8')=='NATIVE WORKSPACE CONTENT'
    assert hashlib.sha256(path.read_bytes()).hexdigest()==digest


def test_failed_native_workspace_write_has_an_incomplete_execution_receipt(native_turn):
    t=native_turn
    sid=t.client.post('/api/workspace/sessions',json={'title':'Write failure'}).raise_for_status().json()['session_id']
    t.scripts.extend([{'tool':'agent_workspace','args':{'operation':'save','area':'files','path':'failed.txt','expected_revision':'wrong','text':'not saved'}},
                     {'text':'Файл сохранён.'}])
    result=t.client.post(PATH,json={'question':'Сохрани файл','session_id':sid,'mode':'agent'},headers=HEADERS).raise_for_status().json()
    receipt=result['artifact']['execution_receipt']
    assert receipt['status']=='incomplete' and receipt['kind']=='agent_file'
    assert result['artifact']['completion_status']=='incomplete' and 'не выполнена' in result['answer']
    assert not list(t.state.rglob('failed.txt'))


def test_physical_pdf_changed_during_generation_never_commits_a_confident_final(native_turn):
    t=native_turn;path,digest=source_pdf(t)
    def changed(body):
        stat=path.stat();raw=path.read_bytes();path.write_bytes(b'X'+raw[1:])
        os.utime(path,ns=(stat.st_atime_ns,stat.st_mtime_ns))
        return {'text':'Уверенный ответ [Источник 1]'}
    t.scripts.extend([{'tool':'read_pdf_source','args':{'doc_id':'registered-document','page':2}},changed])
    stream=t.client.post(PATH+'/stream',json={'question':'Прочти страницу','dataset_ids':['ds'],'session_id':'pdf-changed'},headers=HEADERS).raise_for_status().text
    assert 'SOURCE_REVISION_CHANGED' in stream and 'event: final' not in stream
    with sqlite3.connect(t.database) as db:
        status,success,artifact=db.execute('SELECT crag_status,success,artifact_json FROM chat_history').fetchone()
    assert status=='FAILED' and success==0
    assert json.loads(artifact)['tool_results'][0]['result']['source_sha256']==digest


def test_unindexed_physical_pdf_citation_uses_the_hash_of_the_read_handle(native_turn):
    t=native_turn;_,digest=source_pdf(t)
    with sqlite3.connect(t.database) as db:db.execute("UPDATE documents SET file_hash='' WHERE id='registered-document'")
    t.scripts.extend([{'tool':'read_pdf_source','args':{'doc_id':'registered-document','page':2,'source_sha256':digest}},
                     {'text':'Прочитана страница [Источник 1]'}])
    result=t.client.post(PATH,json={'question':'Прочти страницу','dataset_ids':['ds']},headers=HEADERS).raise_for_status().json()
    source=result['source_map'][0]
    assert source['source_sha256']==digest and 'source_sha256='+digest in source['url']
    assert hashlib.sha256(t.client.get(source['url']).raise_for_status().content).hexdigest()==digest


def test_unindexed_pdf_without_a_revision_stops_before_a_model_followup(native_turn):
    t=native_turn;source_pdf(t)
    with sqlite3.connect(t.database) as db:db.execute("UPDATE documents SET file_hash='' WHERE id='registered-document'")
    t.scripts.extend([{'tool':'read_pdf_source','args':{'doc_id':'registered-document','page':2}},
                     {'text':'This must not be generated.'}])
    stream=t.client.post(PATH+'/stream',json={'question':'Прочти страницу','dataset_ids':['ds']},headers=HEADERS).raise_for_status().text
    assert 'PDF_REVISION_REQUIRED' in stream and 'event: final' not in stream
    assert len(t.seen)==1
