"""Real Axum/SQLite routes and transparent byte streaming to an isolated upstream."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import time

import httpx
import pytest


def free_port():
    with socket.socket() as handle:
        handle.bind(('127.0.0.1',0))
        return handle.getsockname()[1]


def wait_for(action,timeout=10):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        try:
            value=action()
            if value:return value
        except (OSError,httpx.HTTPError):
            pass
        time.sleep(.03)
    raise AssertionError('Isolated native API deadline')


@pytest.fixture
def gateway(tmp_path):
    executable=os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api and set LES_TEST_NATIVE_API for its HTTP/streaming gate')
    upstream_port=free_port();native_port=free_port()
    while native_port==upstream_port:native_port=free_port()
    (tmp_path/'upstream.py').write_text('''import asyncio,hashlib
from pathlib import Path
from fastapi import FastAPI,HTTPException,Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import StreamingResponse,RedirectResponse
app=FastAPI()
app.add_middleware(CORSMiddleware,allow_origins=['http://127.0.0.1:8181'],allow_credentials=True,allow_methods=['*'],allow_headers=['*'])
@app.get('/api/light/instance')
def instance():return {'instance_id':'fixture'}
@app.get('/origin')
def origin(request:Request):return {'base_url':str(request.base_url)}
@app.get('/api/light/native-authorization')
def authorize(request:Request):
    if request.headers.get('x-api-key') not in ('approved-fixture','user-fixture'):raise HTTPException(401,'Authentication required')
    return {'allowed':True}
@app.get('/api/light/native-admin-authorization')
def authorize_admin(request:Request):
    authorize(request)
    if request.headers.get('x-api-key')!='approved-fixture':raise HTTPException(403,'Admin role required')
    return {'allowed':True}
@app.post('/echo')
async def echo(request:Request):
    digest=hashlib.sha256();size=0
    async for block in request.stream():size+=len(block);digest.update(block)
    return {'size':size,'sha256':digest.hexdigest()}
@app.get('/stream')
def stream():
    async def body():
        first='data: {"delta":"Лес 🌲"}\\n\\n'.encode()
        for byte in first:yield bytes([byte])
        await asyncio.sleep(2)
        yield b'event: final\\ndata: {}\\n\\n'
    return StreamingResponse(body(),media_type='text/event-stream')
@app.get('/longstream')
def longstream():
    async def body():
        try:
            while True:
                yield b'data: {}\\n\\n';await asyncio.sleep(.05)
        finally:Path('cancelled.marker').write_text('client disconnected')
    return StreamingResponse(body(),media_type='text/event-stream')
@app.get('/redirect')
def redirect(request:Request):return RedirectResponse(str(request.base_url)+'target?proof=1',status_code=307)
''',encoding='utf-8')
    environment={**os.environ,'PYTHONPATH':str(tmp_path),'PYTHONUTF8':'1'}
    log=(tmp_path/'servers.log').open('wb')
    children=[]
    try:
        children.append(subprocess.Popen([sys.executable,'-B','-m','uvicorn','upstream:app','--host','127.0.0.1','--port',str(upstream_port)],
            cwd=tmp_path,env=environment,stdout=log,stderr=subprocess.STDOUT,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0)))
        database=tmp_path/'Диалоги 日本語.sqlite'
        upstream=f'http://127.0.0.1:{upstream_port}'
        children.append(subprocess.Popen([executable,'--listen',f'127.0.0.1:{native_port}',
            '--upstream',upstream,'--database',str(database),'--state',str(tmp_path/'state')],cwd=tmp_path,stdout=log,stderr=subprocess.STDOUT,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0)))
        url=f'http://127.0.0.1:{native_port}'
        with httpx.Client(base_url=url,timeout=15,trust_env=False,headers={'X-API-Key':'approved-fixture'}) as client:
            wait_for(lambda:client.get('/api/light/instance').status_code==200)
            yield client,database,tmp_path
    finally:
        for child in reversed(children):
            if child.poll() is None:child.kill()
            child.wait(timeout=5)
        log.close()


def test_auth_origin_and_native_route_ownership(gateway):
    client,database,_=gateway
    with httpx.Client(base_url=client.base_url,trust_env=False) as anonymous:
        response=anonymous.post('/api/workspace/sessions',json={'title':'must not write'})
        assert response.status_code==401 and not database.exists()
    response=client.post('/api/workspace/sessions',json={'title':'Лес 🌲'})
    assert response.status_code==200 and response.headers['x-les-handler']=='rust.workspace'
    sid=response.json()['session_id']
    assert client.get(f'/api/workspace/sessions/{sid}').json()['title']=='Лес 🌲'
    assert client.get('/api/light/instance').headers['x-les-handler']=='legacy.proxy'
    assert client.get('/origin').json()['base_url']==str(client.base_url).rstrip('/')+'/'
    assert client.get('/api/workspace/sessions',headers={'Origin':'https://evil.invalid'}).status_code==403
    assert client.get('/api/workspace/sessions',headers={'Host':'evil.invalid'}).status_code==403


def test_native_http_drafts_cas_validation_and_cors(gateway):
    client,_,_=gateway
    created=client.post('/api/workspace/sessions',json={}).json();sid=created['session_id']
    path=f'/api/workspace/sessions/{sid}/draft'
    content={'revision':0,'text':'  日本語 и пробелы 🌲\n\t','attachment':None}
    response=client.put(path,json=content,headers={'Origin':'http://127.0.0.1:8181'})
    assert response.status_code==200 and response.json()=={**content,'revision':1}
    assert response.headers['access-control-allow-origin']=='http://127.0.0.1:8181'
    assert client.put(path,json=content).status_code==409
    assert client.get(path).json()['text']==content['text']
    for change in [{'scope':{'scope_type':'none','dataset_ids':['foreign']}},{'project_id':4}]:
        assert client.patch(f'/api/workspace/sessions/{sid}',json=change).status_code==422
    assert client.post('/api/workspace/sessions',json={'session_id':sid}).status_code==409
    assert client.post('/api/workspace/sessions',json={'project_id':999}).status_code==404
    assert client.put(path,json={**content,'revision':1,'text':'x'*20001}).status_code==422


def test_native_database_wait_does_not_block_http_event_loop(gateway):
    client,database,_=gateway
    sid=client.post('/api/workspace/sessions',json={}).json()['session_id']
    with sqlite3.connect(database) as lock:
        lock.execute('BEGIN IMMEDIATE')
        with ThreadPoolExecutor(max_workers=1) as pool:
            future=pool.submit(client.put,f'/api/workspace/sessions/{sid}/draft',json={'revision':0,'text':'preserved'})
            try:
                time.sleep(.1)
                assert not future.done()
                started=time.monotonic()
                assert client.get('/api/light/instance').status_code==200
                assert time.monotonic()-started<1
            finally:lock.rollback()
            assert future.result(timeout=5).status_code==200


def test_many_native_http_writers_keep_one_draft_revision(gateway):
    client,_,_=gateway
    sid=client.post('/api/workspace/sessions',json={}).json()['session_id']
    path=f'/api/workspace/sessions/{sid}/draft'
    with ThreadPoolExecutor(max_workers=20) as pool:
        responses=list(pool.map(lambda index:client.put(path,json={'revision':0,'text':f'window-{index}'}),range(20)))
    assert [response.status_code for response in responses].count(200)==1
    assert [response.status_code for response in responses].count(409)==19
    assert client.get(path).json()['revision']==1


def test_proxy_streams_utf8_sse_before_the_final_frame(gateway):
    client,_,_=gateway
    started=time.monotonic()
    with client.stream('GET','/stream') as response:
        assert response.status_code==200 and response.headers['x-les-handler']=='legacy.proxy'
        stream=response.iter_raw();first=next(stream)
        assert time.monotonic()-started<1.5
        body=first+b''.join(stream)
    assert body=='data: {"delta":"Лес 🌲"}\n\nevent: final\ndata: {}\n\n'.encode()


def test_stream_cancel_reaches_upstream_and_upload_bytes_are_unchanged(gateway):
    client,_,folder=gateway
    with client.stream('GET','/longstream') as response:
        assert next(response.iter_raw())
    wait_for(lambda:(folder/'cancelled.marker').exists(),5)
    block=bytes(range(256))*1024
    response=client.post('/echo',content=(block for _ in range(8)))
    assert response.status_code==200
    assert response.json()=={'size':len(block)*8,'sha256':hashlib.sha256(block*8).hexdigest()}
    redirect=client.get('/redirect',follow_redirects=False)
    assert redirect.status_code==307 and redirect.headers['location']=='/target?proof=1'


def test_native_projects_keep_admin_boundary_and_project_links(gateway):
    client,database,_=gateway
    with httpx.Client(base_url=client.base_url,trust_env=False,headers={'X-API-Key':'user-fixture'}) as user:
        assert user.post('/api/projects',json={'name':'Forbidden'}).status_code==403
        assert not database.exists()
        assert user.get('/api/projects').status_code==200
    response=client.post('/api/projects',json={'name':'  Корпус 🌲 ', 'code':' Шифр ', 'address':None})
    assert response.status_code==200 and response.headers['x-les-handler']=='rust.catalog'
    project=response.json();pid=project['id']
    assert project['name']=='Корпус 🌲' and project['code']=='Шифр' and project['address']==''
    link=client.post(f'/api/projects/{pid}/links',json={'kind':'dataset','ref':' набор #1 '}).json()
    assert link['ref']=='набор #1'
    assert client.get(f'/api/projects/{pid}').json()['links']==[link]
    assert client.get(f'/api/projects/{pid}/links?kind=dataset').json()=={'links':[link]}
    session=client.post('/api/workspace/sessions',json={'project_id':pid}).json()
    assert session['scope']['dataset_ids']==['набор #1']
    assert client.patch(f'/api/projects/{pid}',json={'status':'unknown'}).status_code==400
    with httpx.Client(base_url=client.base_url,trust_env=False,headers={'X-API-Key':'user-fixture'}) as user:
        assert user.delete(f'/api/projects/{pid}').status_code==403
        assert user.patch(f'/api/projects/{pid}',json={'status':'archived'}).status_code==403
    assert client.request('DELETE',f'/api/projects/{pid}/links',json={'kind':'dataset','ref':'набор #1'}).json()=={'unlinked':True}
    assert client.delete(f'/api/projects/{pid}').json()=={'deleted':True}
    assert client.get(f'/api/projects/{pid}').status_code==404


def test_native_note_scope_update_disable_delete_and_legacy_migration(gateway):
    client,database,_=gateway
    p1=client.post('/api/projects',json={'name':'One'}).json()['id']
    p2=client.post('/api/projects',json={'name':'Two'}).json()['id']
    with sqlite3.connect(database) as conn:
        conn.execute('CREATE TABLE les_notes(id INTEGER PRIMARY KEY,text TEXT NOT NULL,dataset_filter TEXT,created_at REAL,project_id INTEGER,auto INTEGER)')
        conn.executemany("INSERT INTO les_notes VALUES (?,?, '',1,?,1)",[(1,'One',p1),(2,'Two',p2)])
    response=client.get(f'/api/workspace/memory?project_id={p1}')
    assert response.status_code==200 and response.headers['x-les-handler']=='rust.catalog'
    assert [row['id'] for row in response.json()['notes']]==[1]
    path='/api/workspace/memory/1'
    assert client.patch(path,json={'project_id':p2,'text':'leak'}).status_code==404
    changed=client.patch(path,json={'project_id':p1,'text':' 日本語 🌲 ','enabled':False}).json()
    assert changed['text']=='日本語 🌲' and changed['auto']==0 and changed['enabled']==0
    assert client.patch(path,json={'project_id':999,'enabled':True}).status_code==404
    assert client.delete(path,params={'project_id':p2}).status_code==404
    assert client.delete(path).status_code==422
    assert client.delete(path,params={'project_id':p1}).json()=={'deleted':True}
    assert [row['id'] for row in client.get(f'/api/workspace/memory?project_id={p2}').json()['notes']]==[2]


def test_native_context_edit_forget_keeps_history_and_cas(gateway):
    client,database,_=gateway
    assert client.get('/api/workspace/memory/context/absent').status_code==404
    assert not database.exists()
    sid=client.post('/api/workspace/sessions',json={}).json()['session_id']
    path=f'/api/workspace/memory/context/{sid}'
    with sqlite3.connect(database) as conn:
        conn.execute('CREATE TABLE chat_history(id INTEGER PRIMARY KEY,session_id TEXT,question TEXT,answer TEXT,success INTEGER)')
        conn.executemany('INSERT INTO chat_history VALUES (?,?,?,?,?)',[(i,sid,f'Вопрос {i}','Ответ',1) for i in range(1,9)])
    before=client.get(path).json()
    assert before['remembered_turns']==8 and before['revision']==0
    response=client.patch(path,json={'expected_revision':0,'summary':'  日本語 🌲  ','auto_summary':False})
    assert response.status_code==200 and response.headers['x-les-handler']=='rust.catalog'
    assert response.json()['summary']=='日本語 🌲' and response.json()['model_revision']=='user_edit'
    assert client.patch(path,json={'expected_revision':0,'enabled':False}).status_code==409
    assert client.patch(path,json={'expected_revision':1,'summary':'x'*2201}).status_code==422
    forgotten=client.post(path+'/forget',json={'expected_revision':1}).json()
    assert forgotten['cutoff_id']==8 and forgotten['through_id']==8 and forgotten['summary']=='' and forgotten['revision']==2
    assert client.get(path).json()['remembered_turns']==0
    with sqlite3.connect(database) as conn:assert conn.execute('SELECT count(*) FROM chat_history').fetchone()[0]==8


def test_native_agent_state_persists_plain_markdown_and_undo(gateway):
    client,database,folder=gateway
    path='/api/workspace/agent-state'
    assert client.get(path).status_code==200 and not (folder/'state').exists()
    body={'action':'save','area':'user','project_id':0,'expected_revision':'missing','text':'Русский 🌲\n\nПредпочтения 日本語'}
    saved=client.post(path,json=body)
    assert saved.status_code==200 and saved.headers['x-les-handler']=='rust.agent-state'
    assert (folder/'state/agent/USER.md').read_text(encoding='utf-8')==body['text']
    assert client.post(path,json=body).status_code==409
    assert client.get(path,params={'action':'read','area':'user'}).json()['text']==body['text']
    version=client.get(path,params={'action':'version','area':'user','history_id':saved.json()['history_id']}).json()
    assert version['text']==body['text'] and version['actor']=='user'
    response=client.post(path,json={'action':'restore','area':'user','expected_revision':saved.json()['revision'],'history_id':saved.json()['history_id'],'side':'before'})
    assert response.status_code==200 and response.json()['revision']=='missing'
    assert not (folder/'state/agent/USER.md').exists()
    assert not database.exists()


def test_native_deleted_file_is_discoverable_and_export_keeps_utf8_bytes(gateway):
    client,_,folder=gateway
    path='/api/workspace/agent-state'; name='notes/План 日本語.md'; original='\ufeffПлан 🌲\r\n'
    saved=client.post(path,json={'action':'save','area':'files','path':name,'text':original,'expected_revision':'missing'})
    assert saved.status_code==200
    revision=saved.json()['revision']
    exported=client.get(path,params={'action':'export','area':'files','path':name,'expected_revision':revision})
    assert exported.status_code==200 and exported.json()['text'].encode('utf-8')==original.encode('utf-8')
    assert client.get(path,params={'action':'export','area':'files','path':name,'expected_revision':'missing'}).status_code==409
    removed=client.post(path,json={'action':'delete','area':'files','path':name,'expected_revision':revision})
    assert removed.status_code==200 and removed.json()['revision']=='missing'
    listed=client.get(path,params={'action':'list','include_deleted':'true'}).json()['files']
    assert listed[0]['path']==name and listed[0]['exists'] is False
    assert client.get(path,params={'action':'list','include_deleted':'perhaps'}).status_code==422
    restored=client.post(path,json={'action':'restore','area':'files','path':name,'expected_revision':'missing','history_id':removed.json()['history_id'],'side':'before'})
    assert restored.status_code==200
    assert (folder/'state/agent/workspaces/0'/name).read_bytes()==original.encode('utf-8')


def test_native_agent_files_are_scoped_and_never_write_source_paths(gateway):
    client,_,folder=gateway
    p1=client.post('/api/projects',json={'name':'One'}).json()['id']
    p2=client.post('/api/projects',json={'name':'Two'}).json()['id']
    path='/api/workspace/agent-state'
    first=client.post(path,json={'action':'save','project_id':p1,'area':'memory','expected_revision':'missing','text':'Only One'})
    assert first.status_code==200
    bundle=client.get(path,params={'project_id':p2}).json()
    assert all(d['text']!='Only One' for d in bundle['documents'])
    assert client.get(path,params={'project_id':9999}).status_code==404
    source=folder/'source.txt';source.write_text('Read-only source')
    for bad in ['../source.txt',str(source),'a:secret','CON.txt']:
        assert client.post(path,json={'action':'save','area':'files','path':bad,'expected_revision':'missing','text':'overwrite'}).status_code==422
    assert source.read_text()=='Read-only source'
    work=client.post(path,json={'action':'save','area':'files','path':'notes/План #1.md','expected_revision':'missing','text':'Before 日本語'})
    assert work.status_code==200
    changed=client.post(path,json={'action':'edit','area':'files','path':'notes/План #1.md','expected_revision':work.json()['revision'],'old_text':'Before','new_text':'After'})
    assert changed.status_code==200
    assert client.get(path,params={'action':'list'}).json()['files'][0]['path']=='notes/План #1.md'
