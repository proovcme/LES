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
from urllib.parse import quote

import httpx
import pytest

MODEL_BRIDGE_TOKEN = '1' * 64


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


def static_frontend(folder):
    frontend = folder/'Интерфейс с пробелами'
    (frontend/'assets').mkdir(parents=True)
    files = {
        'index.html': '<html lang="ru"><title>Лес 🌲</title></html>',
        'assets/main.js': 'console.log("Лес 🌲");',
        'assets/main.css': 'body { color: green; }',
        'assets/Файл с пробелами.svg': '<svg xmlns="http://www.w3.org/2000/svg"/>',
    }
    for name, content in files.items():
        (frontend/name).write_bytes(content.encode('utf-8'))
    (frontend/'build-receipt.json').write_text(json.dumps({
        'schema':'les.frontend.v1',
        'files': {name:hashlib.sha256(content.encode('utf-8')).hexdigest() for name, content in files.items()},
    }), encoding='utf-8')
    (frontend/'private.txt').write_text('unlisted fixture')
    return frontend


@pytest.fixture
def gateway(tmp_path):
    executable=os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api and set LES_TEST_NATIVE_API for its HTTP/streaming gate')
    upstream_port=free_port();native_port=free_port()
    while native_port==upstream_port:native_port=free_port()
    (tmp_path/'upstream.py').write_text('''import asyncio,hashlib,json,os
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
@app.get('/api/light/native-auth-policy')
def auth_policy():
    source=Path('native-auth-fixture.json')
    if not source.is_file():raise HTTPException(404)
    with Path('policy-calls.log').open('a') as log:log.write('bootstrap\\n')
    return json.loads(source.read_text(encoding='utf-8'))
@app.post('/test-stop-upstream')
async def stop_upstream():
    asyncio.get_running_loop().call_later(.1, os._exit, 0)
    return {}
@app.get('/api/light/native-authorization')
def authorize(request:Request):
    Path('legacy-authorization.marker').touch()
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
        frontend=static_frontend(tmp_path)
        children.append(subprocess.Popen([executable,'--listen',f'127.0.0.1:{native_port}',
            '--upstream',upstream,'--database',str(database),'--state',str(tmp_path/'state'),
            '--frontend-assets',str(frontend),'--legacy-ui-url','http://127.0.0.1:55444/classic'],cwd=tmp_path,stdout=log,stderr=subprocess.STDOUT,
            env={**os.environ,'LES_INTERNAL_MODEL_TOKEN':MODEL_BRIDGE_TOKEN},
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


def test_private_model_bridge_rejects_browser_and_out_of_scope_targets(gateway):
    client, _, _ = gateway
    path = '/api/light/internal-model-http'
    target = 'http://127.0.0.1:11434/api/chat'
    token = {'X-Les-Model-Token': MODEL_BRIDGE_TOKEN}
    assert client.post(path, headers={'X-Les-Model-Target': target}, json={}).status_code == 404
    assert client.post(path, headers={**token, 'Origin': str(client.base_url).rstrip('/'),
                                      'X-Les-Model-Target': target}, json={}).status_code == 404
    for target in (
        'http://192.0.2.1:11434/api/chat', 'https://127.0.0.1:11434/api/chat',
        'http://127.0.0.1:11434/api/tags', 'http://user@127.0.0.1:11434/api/chat',
        'http://127.0.0.1:11434/api/chat?redirect=1', 'http://127.1:11434/api/chat',
        str(client.base_url.join('/api/chat')),
    ):
        response = client.post(path, headers={**token, 'X-Les-Model-Target': target}, json={})
        assert response.status_code == 400
        assert response.headers['x-les-model-bridge-error'] == 'MODEL_BRIDGE_TARGET_REJECTED'


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


def test_native_shell_and_assets_work_after_upstream_stops(gateway):
    client, _, _ = gateway
    from proxy.light_frontend import CSP

    configuration = client.get('/api/light/frontend')
    assert configuration.headers['x-les-handler'] == 'rust.frontend'
    assert configuration.json() == {'legacy_url':'http://127.0.0.1:55444/classic'}
    assert client.post('/test-stop-upstream').status_code == 200
    wait_for(lambda: client.get('/api/light/instance').status_code == 503)
    redirect = client.get('/app?session=сессия', follow_redirects=False)
    assert redirect.status_code == 307
    assert redirect.headers['location'] == '/app/?session=%D1%81%D0%B5%D1%81%D1%81%D0%B8%D1%8F'
    page = client.get(redirect.headers['location'])
    assert page.status_code == 200 and 'Лес 🌲' in page.text
    assert page.headers['x-les-handler'] == 'rust.frontend'
    assert page.headers['content-security-policy'] == CSP
    assert page.headers['cache-control'] == 'no-store'
    js = client.get('/app/assets/main.js')
    assert js.headers['content-type'].startswith('text/javascript')
    assert 'Лес 🌲' in js.text
    assert client.get('/app/assets/main.css').headers['content-type'].startswith('text/css')
    assert client.get('/app/assets/Файл с пробелами.svg').headers['content-type'] == 'image/svg+xml'
    head = client.head('/app/assets/main.js')
    assert head.status_code == 200 and not head.content
    assert int(head.headers['content-length']) == len(js.content)
    assert client.get('/app/assets/main.js', headers={'If-None-Match':js.headers['etag']}).status_code == 304
    assert client.get('/app/private.txt').status_code == 404
    assert client.get('/app/%2e%2e/private.txt').status_code == 404
    assert client.get('/app/assets%5Cmain.js').status_code == 404
    assert client.post('/app/assets/main.js').status_code == 405
    assert client.get('/app/', headers={'Origin':'https://evil.invalid'}).status_code == 403
    forest = client.get('/qdrant-visualizer/index.html')
    assert forest.headers['x-les-handler'] == 'rust.frontend'
    assert 'data-native-forest="true"' in forest.text
    assert client.head('/qdrant-visualizer/forest-scene.js').status_code == 200
    assert client.get('/qdrant-visualizer/private.txt').status_code == 404
    assert client.get('/classic?tab=chat').headers['location'] == '/app/'
    assert client.get('/classic?tab=data').headers['location'] == 'http://127.0.0.1:55444/classic?tab=data'


def test_native_library_visibility_pagination_and_create_permissions(gateway):
    client, database, root_path = gateway
    with sqlite3.connect(database) as conn:
        conn.executescript("""
        CREATE TABLE datasets(id TEXT PRIMARY KEY,name TEXT,status TEXT,dataset_scope TEXT,module_id TEXT,chunk_count INT DEFAULT 0);
        CREATE TABLE documents(id TEXT PRIMARY KEY,dataset_id TEXT,file_name TEXT,status TEXT,file_size INT,chunk_count INT);
        INSERT INTO datasets VALUES('fixture','Проект 日本語','IDLE','user','',7),('hidden','Private','IDLE','system','',90);
        INSERT INTO documents VALUES('public','fixture','Раздел/ВЕДОМОСТЬ #?.xlsx','INDEXED',321,7),
          ('temporary','fixture','Раздел/~$owner.xlsx','PENDING',123,0),('secret','hidden','private.pdf','INDEXED',123,90);
        """)
    root = '/api/light/library'
    read = client.get(root)
    assert read.headers['x-les-handler'] == 'rust.library'
    assert [row['id'] for row in read.json()['datasets']] == ['fixture']
    assert read.json()['datasets'][0]['document_count'] == 1
    documents = client.get(root, params={'dataset_id':'fixture', 'q':'ведомость #?', 'limit':1}).json()
    assert documents['total'] == 1 and documents['documents'][0]['file_size'] == 321
    assert 'source_path' not in documents['documents'][0]
    assert client.get(root, params={'dataset_id':'fixture','offset':1}).json()['documents'] == []
    assert client.get(root, params={'dataset_id':'hidden'}).status_code == 404
    assert {n['id'] for n in client.get('/api/light/forest').json()['nodes']} == {
        'ds:fixture','fixture:Раздел/ВЕДОМОСТЬ #?.xlsx',
    }
    assert client.post('/api/rag/datasets', json={'name':'should fail'}, headers={'X-API-Key':'user-fixture'}).status_code == 403
    assert client.post('/api/rag/datasets', json={'name':'  '}).status_code == 400
    saved = client.post('/api/rag/datasets', json={'name':' Сборка 🌲 '}).raise_for_status().json()
    assert saved['name'] == 'Сборка 🌲'
    from uuid import UUID
    assert UUID(saved['id']).version == 4
    assert client.get(root, params={'dataset_id':saved['id']}).json()['total'] == 0
    jobs = root_path/'state/data/les_meta.db'
    jobs.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(jobs) as conn:
        conn.executescript("""
        CREATE TABLE jobs(id TEXT,type TEXT,status TEXT,dataset_id TEXT,processed INT,total INT,message TEXT);
        INSERT INTO jobs VALUES('current','dataset_intake','running','fixture',3,9,'absolute source path'),
          ('hidden','dataset_intake','running','hidden',5,9,'secret'),
          ('wrong-type','other','running','fixture',4,9,'secret');
        """)
    job = client.get('/api/light/intake?id=current').raise_for_status().json()
    assert job['processed'] == 3 and job['total'] == 9 and 'message' not in job
    assert client.get('/api/light/intake?id=hidden').status_code == 404
    assert client.get('/api/light/intake?id=wrong-type').status_code == 404


def test_corrupt_frontend_receipt_fails_closed(tmp_path):
    executable = os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api to verify asset integrity')
    frontend = static_frontend(tmp_path)
    (frontend/'assets/main.js').write_text('changed after build')
    listen = free_port()
    upstream = free_port()
    while upstream == listen:
        upstream = free_port()
    process = subprocess.run([executable, '--listen', f'127.0.0.1:{listen}',
        '--upstream', f'http://127.0.0.1:{upstream}', '--database', str(tmp_path/'workspace.sqlite'),
        '--state', str(tmp_path/'state'), '--frontend-assets', str(frontend)],
        cwd=tmp_path, capture_output=True, timeout=10)
    assert process.returncode == 1 and b'FRONTEND_ASSET_INVALID' in process.stderr
    assert not (tmp_path/'workspace.sqlite').exists()


@pytest.mark.parametrize('trusted_role', [None, 'admin'])
def test_native_auth_policy_keys_revocation_and_upstream_independence(gateway, trusted_role):
    client, database, folder = gateway
    (folder/'native-auth-fixture.json').write_text(json.dumps({
        'schema': 'les.native-auth-policy.v1', 'trusted_role': trusted_role,
        'compatible': True, 'cors_origins': [], 'cors_loopback': True,
        'proxy_header': 'x-les-trusted-network',
    }), encoding='utf-8')
    registry = folder/'state/data/les_meta.db'
    registry.parent.mkdir(parents=True)
    with sqlite3.connect(registry) as db:
        db.execute('CREATE TABLE auth_keys (key_value TEXT PRIMARY KEY, role TEXT, expires_at TEXT, is_active INTEGER)')
        db.executemany('INSERT INTO auth_keys VALUES (?,?,?,?)', [
            ('approved-fixture', 'admin', '2099-01-01 12:00:00', 1),
            ('user-fixture', 'user', '2099-01-01T12:00:00.123456', 1),
            ('expired-fixture', 'user', '2001-01-01T00:00:00', 1),
            ('disabled-fixture', 'user', None, 0),
            ('les-admin-protected-fixture', 'user', '2001-01-01T00:00:00', 1),
        ])
    with httpx.Client(base_url=client.base_url, trust_env=False) as request:
        sessions = '/api/workspace/sessions'
        assert request.get('/api/light/native-auth-policy').status_code == 404
        if not trusted_role:
            denied = request.get('/api/auth/keys')
            assert denied.status_code == 401
            assert denied.headers['x-les-handler'] == 'rust.auth-keys'
        default = request.get(sessions)
        assert default.status_code == (200 if trusted_role else 401)
        assert not database.exists()
        for key, error in [('expired-fixture', 'API key expired'), ('disabled-fixture', 'Invalid or disabled API key')]:
            response = request.get(sessions, headers={'X-API-Key': key})
            assert response.status_code == (200 if trusted_role else 401)
            if not trusted_role:
                assert response.json()['detail'] == error
        user = {'Authorization': 'bEaReR user-fixture', 'Origin': 'http://127.0.0.1:8181'}
        response = request.post('/api/projects', headers=user, json={'name': 'Forbidden'})
        assert response.status_code == 403 and not database.exists()
        assert response.headers['access-control-allow-origin'] == user['Origin']
        assert response.headers['access-control-allow-credentials'] == 'true'
        assert response.headers['vary'] == 'Origin'
        assert request.get(sessions, headers=user).status_code == 200
        assert request.post('/api/projects', headers={'X-API-Key': 'les-admin-protected-fixture'},
                            json={'name': 'Контроль'}).status_code == 200
        with sqlite3.connect(registry) as db:
            db.execute("UPDATE auth_keys SET role='public' WHERE key_value='user-fixture'")
        assert request.get(sessions, headers=user).status_code == 403
        assert not (folder/'legacy-authorization.marker').exists()
        assert client.get(sessions, headers={'X-Forwarded-For': '127.0.0.1'}).status_code == 200
        assert (folder/'legacy-authorization.marker').is_file()
        assert (folder/'policy-calls.log').read_text().splitlines() == ['bootstrap']
        assert client.post('/test-stop-upstream').status_code == 200
        wait_for(lambda: client.get('/api/light/instance').status_code == 503)
        created = client.post(sessions, json={'title': 'Живой диалог 🌲'})
        assert created.status_code == 200
        sid = created.json()['session_id']
        assert client.get(f'{sessions}/{sid}').json()['title'] == 'Живой диалог 🌲'


def test_native_key_management_preserves_roles_and_serializes_device_binding(gateway):
    gateway_client, _, folder = gateway
    (folder/'native-auth-fixture.json').write_text(json.dumps({
        'schema': 'les.native-auth-policy.v1', 'trusted_role': 'admin',
        'compatible': True, 'cors_origins': [], 'cors_loopback': True,
        'proxy_header': 'x-les-trusted-network',
    }), encoding='utf-8')
    registry = folder/'state/data/les_meta.db'
    registry.parent.mkdir(parents=True)
    with sqlite3.connect(registry) as db:
        db.execute("""CREATE TABLE auth_keys (
            key_value TEXT PRIMARY KEY, holder_name TEXT NOT NULL DEFAULT '',
            role TEXT NOT NULL DEFAULT 'user', is_active INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL DEFAULT (datetime('now','localtime'))
        )""")
        db.execute("INSERT INTO auth_keys (key_value,holder_name) VALUES ('previous-fixture','Прежний ключ')")
    with httpx.Client(base_url=gateway_client.base_url, trust_env=False) as client:
        keys, verify = '/api/auth/keys', '/api/auth/verify'
        listing = client.get(keys, headers={'Origin': 'http://127.0.0.1:8181'})
        assert listing.status_code == 200 and listing.headers['x-les-handler'] == 'rust.auth-keys'
        assert listing.headers['cache-control'] == 'no-store'
        assert listing.headers['access-control-allow-origin'] == 'http://127.0.0.1:8181'
        assert listing.json()[0]['holder_name'] == 'Прежний ключ'
        assert listing.json()[0]['device_bound'] == 0 and listing.json()[0]['expires_at'] is None
        assert client.post('/test-stop-upstream').status_code == 200
        wait_for(lambda: client.get('/api/light/instance').status_code == 503)

        def create(key, role='user', **options):
            return client.post(keys, json={'key_value': key, 'role': role, **options})

        plain = client.post(keys, content='{"key_value":"plain-fixture"}', headers={
            'Content-Type': 'text/plain', 'Origin': 'http://127.0.0.1:8181',
        })
        assert plain.status_code == 422
        assert client.post(verify, json={'key':'plain-fixture'}).status_code == 401
        assert create('ordinary-admin-fixture', 'admin').status_code == 200
        protected = create('les-admin-root-fixture', expires_days=1)
        assert protected.json()['role'] == 'admin' and protected.json()['expires_at'] is None
        admin = {'X-API-Key': 'ordinary-admin-fixture'}
        root = {'X-API-Key': 'les-admin-root-fixture'}
        assert client.post(keys, headers=admin, json={'key_value':'denied-root','role':'admin'}).status_code == 403
        assert client.post(keys, headers=root, json={'key_value':'granted-admin','role':'admin'}).status_code == 200
        assert client.post(f'{keys}/toggle', headers=root,
                           json={'key_value':'les-admin-root-fixture','is_active':0}).status_code == 403
        assert client.get(keys, headers={'X-API-Key':'previous-fixture'}).status_code == 403

        key = 'test key # 日本語'
        created = create(key, holder_name='  Читатель  ', expires_days='2')
        assert created.status_code == 200 and created.json()['expires_at']
        assert create(key).status_code == 409
        assert create(key + ' bad', expires_days=None).status_code == 422
        with ThreadPoolExecutor(max_workers=2) as pool:
            replies = list(pool.map(lambda device: client.post(verify, json={'key':key,'fingerprint':device}),
                                    ['first', 'second']))
        assert sorted(item.status_code for item in replies) == [200, 403]
        assert next(item for item in replies if item.is_success).json()['holder'] == 'Читатель'
        bound = next(row for row in client.get(keys).json() if row['key_value'] == key)
        assert bound['device_bound'] == 1
        assert client.post(f'{keys}/reset-device', json={'key_value':key}).status_code == 200
        assert client.post(verify, json={'key':key,'fingerprint':'new-device'}).status_code == 200
        assert client.post(f'{keys}/toggle', json={'key_value':key,'is_active':0}).status_code == 200
        assert client.post(verify, json={'key':key}).status_code == 401
        assert client.post(f'{keys}/delete', json={'key_value':key}).json()['status'] == 'deleted'
        path_key = 'ключ # 日本語'
        assert create(path_key).status_code == 200
        assert client.delete(f'{keys}/{quote(path_key, safe="")}').json()['status'] == 'deleted'
        protected_key = 'les-admin-root-fixture'
        assert client.delete(f'{keys}/{quote(protected_key, safe="")}', headers=admin).status_code == 403
        assert client.delete(f'{keys}/{quote(protected_key, safe="")}').json()['status'] == 'deleted'
        assert not (folder/'legacy-authorization.marker').exists()


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
