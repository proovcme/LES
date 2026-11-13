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


def native_trust(folder, role=None):
    (folder/'state/.env').write_text('TRUSTED_NETWORKS=' + ('127.0.0.1/32' if role else '')
                                   + '\nTRUSTED_NETWORK_ROLE=' + (role or 'admin') + '\n', encoding='utf-8')


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
    with Path('policy-calls.log').open('a') as log:log.write('bootstrap\\n')
    return {'schema':'les.native-auth-policy.v1','compatible':True,'trusted_role':'admin',
            'cors_origins':[],'cors_loopback':True,'proxy_header':'x-les-trusted-network'}
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
        (tmp_path/'state/data').mkdir(parents=True)
        native_trust(tmp_path)
        with sqlite3.connect(tmp_path/'state/data/les_meta.db') as db:
            db.execute('CREATE TABLE auth_keys(key_value TEXT PRIMARY KEY,role TEXT,expires_at TEXT,is_active INTEGER)')
            db.executemany('INSERT INTO auth_keys VALUES(?,?,?,?)',[
                ('approved-fixture','admin',None,1),('user-fixture','user',None,1)])
        children.append(subprocess.Popen([sys.executable,'-B','-m','uvicorn','upstream:app','--host','127.0.0.1','--port',str(upstream_port)],
            cwd=tmp_path,env=environment,stdout=log,stderr=subprocess.STDOUT,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0)))
        database=tmp_path/'Диалоги 日本語.sqlite'
        upstream=f'http://127.0.0.1:{upstream_port}'
        frontend=static_frontend(tmp_path)
        children.append(subprocess.Popen([executable,'--listen',f'127.0.0.1:{native_port}',
            '--upstream',upstream,'--database',str(database),'--state',str(tmp_path/'state'),
            '--frontend-assets',str(frontend),'--legacy-ui-url','http://127.0.0.1:55444/classic'],cwd=tmp_path,stdout=log,stderr=subprocess.STDOUT,
            env={**os.environ,'LES_INTERNAL_MODEL_TOKEN':MODEL_BRIDGE_TOKEN,
                 'LES_ENV_PATH':str(tmp_path/'state/.env')},
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


def test_rejected_model_posts_deliver_errors_and_leave_the_api_usable(gateway):
    client, _, _ = gateway
    path='/api/light/internal-model-http'
    for i in range(64):
        denied=client.post(path,headers={'Origin':str(client.base_url).rstrip('/'),
            'X-Les-Model-Token':MODEL_BRIDGE_TOKEN,'X-Les-Model-Target':'http://127.0.0.1:11434/api/chat'},
            json={'untrusted':'Данные 日本語 '+str(i)})
        assert denied.status_code==404 and denied.json()['detail']=='Not Found'
        invalid=client.post(path,headers={'X-Les-Model-Token':MODEL_BRIDGE_TOKEN,
            'X-Les-Model-Target':'http://192.0.2.1:11434/api/chat'},json={'untrusted':str(i)})
        assert invalid.status_code==400
        assert invalid.headers['x-les-model-bridge-error']=='MODEL_BRIDGE_TARGET_REJECTED'
    assert client.get('/api/workspace/sessions').status_code==200


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
    native_trust(folder, trusted_role)
    registry = folder/'state/data/les_meta.db'
    registry.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(registry) as db:
        db.execute('DROP TABLE auth_keys')
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
        assert not (folder/'legacy-authorization.marker').exists()
        assert not (folder/'policy-calls.log').exists()
        foreign = request.get(sessions, headers={'X-Forwarded-For':'192.0.2.10'})
        assert foreign.status_code == 401
        assert client.post('/test-stop-upstream').status_code == 200
        wait_for(lambda: client.get('/api/light/instance').status_code == 503)
        created = client.post(sessions, json={'title': 'Живой диалог 🌲'})
        assert created.status_code == 200
        sid = created.json()['session_id']
        assert client.get(f'{sessions}/{sid}').json()['title'] == 'Живой диалог 🌲'


@pytest.mark.parametrize('settings,code', [
    ("TRUSTED_NETWORKS='broken\n", 'AUTH_SETTINGS_INVALID'),
    ('TRUSTED_NETWORKS=${OTHER_NETWORKS}\n', 'AUTH_SETTINGS_EXPANSION_UNSUPPORTED'),
    ('CORS_ALLOWED_ORIGIN_REGEX="(?=unsupported)"\n', 'AUTH_CORS_PATTERN_UNSUPPORTED'),
    ('TRUSTED_PROXY_HEADER="bad header"\n', 'AUTH_PROXY_HEADER_INVALID'),
])
def test_native_auth_invalid_settings_never_delegate_or_grant_default_access(gateway, settings, code):
    client, database, folder = gateway
    (folder/'state/.env').write_text(settings, encoding='utf-8')
    response = client.get('/api/workspace/sessions')
    assert response.status_code == 503 and not database.exists()
    assert response.json()['code'] == code and response.json()['detail']
    assert code in (folder/'servers.log').read_text(encoding='utf-8', errors='replace')
    assert not (folder/'legacy-authorization.marker').exists()
    assert not (folder/'policy-calls.log').exists()
    # A rejected configuration is not cached as a permissive policy. Repairing
    # the owned settings allows the same native process to retry safely.
    native_trust(folder)
    assert client.get('/api/workspace/sessions').status_code == 200


def test_native_auth_custom_policy_forwarding_and_cors_without_legacy_calls(gateway):
    client, _, folder = gateway
    (folder/'state/.env').write_text(
        "export\tTRUSTED_NETWORKS='192.0.2.0/24'\nTRUSTED_NETWORK_ROLE=user\n"
        "TRUSTED_PROXY_HEADER=x-custom-trust\nCORS_ALLOWED_ORIGINS=\n"
        r"CORS_ALLOWED_ORIGIN_REGEX='https?://127\.0\.0\.1:818[12]'" + '\n', encoding='utf-8')
    with httpx.Client(base_url=client.base_url, trust_env=False) as request:
        sessions = '/api/workspace/sessions'
        assert request.get(sessions).status_code == 401
        assert request.get(sessions, headers={'X-Forwarded-For':'192.0.2.8, 127.0.0.1'}).status_code == 200
        assert request.post('/api/projects', json={'name':'Denied'}, headers={'X-Forwarded-For':'192.0.2.8'}).status_code == 403
        assert request.get(sessions, headers={'X-Real-IP':'192.0.2.9'}).status_code == 200
        assert request.get(sessions, headers={'X-Custom-Trust':'true'}).status_code == 200
        assert request.get(sessions, headers={'X-Les-Trusted-Network':'true'}).status_code == 401
        assert request.get(sessions, headers={'X-Forwarded-For':b'\xff'}).status_code == 401
        for port, allowed in [(8181, True), (8182, True), (8183, False)]:
            origin = f'http://127.0.0.1:{port}'
            response = request.get(sessions, headers={'X-Forwarded-For':'192.0.2.8','Origin':origin})
            assert response.status_code == 200
            assert (response.headers.get('access-control-allow-origin') == origin) == allowed
            preflight = request.options(sessions, headers={'Origin':origin,'Access-Control-Request-Method':'POST',
                                                         'Access-Control-Request-Headers':'Content-Type,X-API-Key'})
            assert preflight.status_code == (200 if allowed else 400)
            if allowed:
                assert preflight.headers['x-les-handler'] == 'rust.auth-policy'
                assert preflight.headers['access-control-allow-origin'] == origin
                assert preflight.headers['access-control-allow-headers'] == 'Content-Type,X-API-Key'
        assert request.options(sessions, headers={'Origin':'http://127.0.0.1:8181',
                                                  'Access-Control-Request-Method':'BREW'}).status_code == 400
    assert not (folder/'legacy-authorization.marker').exists()
    assert not (folder/'policy-calls.log').exists()


def test_invalid_media_upload_does_not_break_the_next_keepalive_request(gateway):
    gateway_client,_,folder=gateway
    native_trust(folder,'admin')
    with httpx.Client(base_url=gateway_client.base_url,trust_env=False,timeout=10) as client:
        def parts():
            yield b'not-json-first-part'
            time.sleep(.1)
            yield b'-last-part'
        for _ in range(8):
            result=client.post('/api/auth/keys',content=parts(),headers={'Content-Type':'text/plain'})
            assert result.status_code==422
            assert client.get('/api/light/instance').status_code==200


def test_native_key_management_preserves_roles_and_serializes_device_binding(gateway):
    gateway_client, _, folder = gateway
    native_trust(folder, 'admin')
    registry = folder/'state/data/les_meta.db'
    registry.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(registry) as db:
        db.execute('DROP TABLE auth_keys')
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


def test_native_context_report_and_isolation_are_scoped_and_redact_private_sections(gateway):
    from dataclasses import replace
    from proxy.services.chat_evidence_context import (
        confirm_context_delivery, govern_inference_messages, prepared_context_trace,
    )
    from proxy.services.model_execution_preset_service import _FACTORY_9B

    client, database, _ = gateway
    sid = client.post('/api/workspace/sessions', json={}).json()['session_id']
    other = client.post('/api/workspace/sessions', json={}).json()['session_id']
    path = f'/api/workspace/memory/context/{sid}'
    visible_text = '[Источник 1] Лист проекта\nПроверяемая цитата 🌲'
    evidence = (visible_text, '[Источник 2] ' + 'Длинный файл ' * 5000)
    source_map = [
        {'index': 1, 'doc_name': 'Проект 東京.pdf', 'source_page': 9, 'locator': {'source_sha256': 'a'*64, 'path': 'private-path'}},
        {'index': 2, 'doc_name': 'Длинный файл.docx'},
    ]
    _, packet = govern_inference_messages(
        preset=replace(_FACTORY_9B, input_token_limit=4000), profile_prefix='private-profile',
        request_payload='private-question', evidence=evidence, source_map=source_map,
    )
    call = prepared_context_trace(packet, purpose='answer', source_map=source_map, evidence=evidence)
    prepared = dict(call)
    confirm_context_delivery(call, {'prompt_tokens': 234})
    call['produced_answer'] = True
    trace = {'context_governor': {'calls': [call, prepared]}}
    with sqlite3.connect(database) as conn:
        conn.execute('CREATE TABLE chat_history(id INTEGER PRIMARY KEY,session_id TEXT,question TEXT,answer TEXT,retrieval_trace_json TEXT)')
        conn.execute('INSERT INTO chat_history VALUES (1,?,?,?,?)', (sid, 'Вопрос', 'Ответ', json.dumps(trace)))
    assert client.get(f'/api/workspace/memory/context/{other}/report?history_id=1').status_code == 404
    assert client.put(f'/api/workspace/memory/context/{other}/messages',
                      json={'history_id': 1, 'role': 'assistant', 'excluded': True}).status_code == 404
    response = client.get(path + '/report', params={'history_id': 1})
    assert response.status_code == 200 and response.headers['x-les-handler'] == 'rust.catalog'
    actual, unused = response.json()['calls']
    assert actual['delivery'] == 'confirmed' and actual['produced_answer'] and actual['provider_prompt_tokens'] == 234
    assert unused['delivery'] == 'prepared' and unused['provider_prompt_tokens'] is None
    assert actual['sources'][0]['model_excerpt'] == visible_text
    assert actual['sources'][0]['source_page'] == 9
    assert actual['omitted_sources'] == [{'index': 2, 'doc_name': 'Длинный файл.docx'}]
    assert actual['free'] == actual['budget'] - actual['used']
    assert 'private-' not in response.text
    assert 'Длинный файл Длинный файл' not in response.text
    assert client.get(path + '/messages').json() == []
    change = {'history_id': 1, 'role': 'assistant', 'excluded': True}
    assert client.put(path + '/messages', json=change).json() == [{'history_id': 1, 'role': 'assistant'}]
    assert client.put(path + '/messages', json=change).status_code == 200
    assert client.get(path).json()['revision'] == 1
    assert client.get(path + '/messages').json() == [{'history_id': 1, 'role': 'assistant'}]
    assert client.put(path + '/messages', json={**change, 'excluded': False}).json() == []


def test_native_agent_state_persists_plain_markdown_and_undo(gateway):
    client,database,folder=gateway
    path='/api/workspace/agent-state'
    def state_files():
        return {p.relative_to(folder/'state'):p.read_bytes() for p in (folder/'state').rglob('*') if p.is_file()}
    before = state_files()  # The fixture now owns its real native key registry.
    assert client.get(path).status_code==200 and state_files()==before
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


def test_native_model_listing_and_history_survive_upstream_stop(gateway):
    from datetime import datetime, timedelta, timezone
    from backend.workspace_native import catalog_call
    from proxy.services.model_connection_contracts import (
        CapabilityName, CapabilityObservation, CapabilitySnapshot, CapabilityState,
        ConnectionLocality, ConnectionRole,
    )
    from proxy.services.model_connection_registry_service import ModelConnectionRegistry
    from proxy.routers.model_connections import _admin_projection
    from proxy.services.model_secret_service import EnvironmentSecretStore
    client, database, folder = gateway
    native_trust(folder)
    # This fixture owns its keys and model registry; it never reads the working LES.
    key_path = folder/'state/data/les_meta.db'
    key_path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(key_path) as db:
        db.execute('DROP TABLE auth_keys')
        db.execute('CREATE TABLE auth_keys(key_value TEXT,role TEXT,expires_at TEXT,is_active INTEGER)')
        db.executemany('INSERT INTO auth_keys VALUES (?,?,?,?)',[
            ('approved-fixture','admin',None,1),('user-fixture','user',None,1)])
    env_path = folder/'state/.env'
    env_path.write_text('TRUSTED_NETWORKS=\nLES_MODEL_CONNECTION_TEST_API_KEY=fixture-never-return-this\n', encoding='utf-8')
    registry = ModelConnectionRegistry(database)
    revision = registry.create_connection(display_name='Модель 日本語 🌲',base_url='http://127.0.0.1:11434/v1',
        model_id='fixture:9b',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=24576,
        secret_ref='env:LES_MODEL_CONNECTION_TEST_API_KEY',extension_type='ollama',actor='admin:test')
    registry.bind_role(ConnectionRole.ANSWER, revision.revision_id, expected_binding_revision=None, actor='admin:test')
    observed = datetime(2026,10,1,tzinfo=timezone.utc)
    registry.save_capability_snapshot(CapabilitySnapshot('cap:fixture',revision.revision_id,(
        CapabilityObservation(CapabilityName.CHAT_COMPLETIONS,CapabilityState.SUPPORTED,'probe',observed,'http_200'),
    ),observed,observed+timedelta(days=1),{}),actor='admin:test')
    secrets = EnvironmentSecretStore(env_path, environ={})
    expected = _admin_projection(revision, registry=registry, secret_store=secrets).model_dump()
    # Bootstrap native auth once, then stop the retained test backend completely.
    templates = client.get('/api/model-connections/templates').raise_for_status()
    assert templates.headers['x-les-handler']=='rust.model-registry'
    assert any(t['template_id']=='ollama' for t in templates.json()['templates'])
    assert client.post('/test-stop-upstream').status_code==200
    wait_for(lambda:client.get('/api/light/instance').status_code==503)
    listing=client.get('/api/model-connections').raise_for_status()
    assert listing.headers['x-les-handler']=='rust.model-registry'
    assert listing.headers['cache-control']=='no-store'
    assert listing.json()['connections']==[expected]
    assert listing.json()['bindings']['answer']['connection_revision_id']==revision.revision_id
    assert 'secret_ref' not in listing.text and 'fixture-never-return-this' not in listing.text
    assert client.get('/api/model-connections',headers={'X-API-Key':'user-fixture'}).status_code==403
    # Legacy schemas migrate in Rust and keep a failed tool read distinct from success.
    with sqlite3.connect(database) as db:
        db.execute('CREATE TABLE chat_history(id INTEGER PRIMARY KEY,timestamp TEXT,question TEXT,answer TEXT,sources TEXT,crag_status TEXT,latency_sec REAL,tokens INTEGER,session_id TEXT)')
        db.execute("INSERT INTO chat_history VALUES(1,'2026-10-01','Вопрос 日本語','Ответ','doc:1','UNVALIDATED',1,2,'chat:fixture')")
    messages=client.get('/api/chat/history',params={'session_id':'chat:fixture'}).raise_for_status()
    assert messages.headers['x-les-handler']=='rust.chat-history'
    assert messages.json()[-1]['text']=='Ответ'
    assert client.get('/api/chat/learning').json()[0]['id']==1
    assert client.get('/api/chat/learning',params={'confirmed_only':True}).json()==[]
    key=catalog_call(database,'chat_history_begin',safe_fallback='',session_id='chat:fixture',question='Прочитай PDF',attachment_context='')
    failure={'code':'PDF_REVISION_CHANGED','detail':'PDF изменился. Проверка остановлена.'}
    catalog_call(database,'chat_history_checkpoint',id=key,event='error',error=failure)
    answer=client.get('/api/chat/history',params={'session_id':'chat:fixture'}).json()[-1]
    assert answer['text']=='' and answer['crag']=='FAILED' and answer['meta']['error']==failure
    assert [row['id'] for row in client.get('/api/chat/learning').json()]==[1]
    sessions=client.get('/api/chat/sessions').raise_for_status().json()
    assert sessions[0]['session_id']=='chat:fixture' and sessions[0]['msg_count']==2
    assert client.get('/api/chat/history?limit=oops').status_code==422
    assert client.get('/api/chat/learning?confirmed_only=oops').status_code==422


def test_native_cold_start_and_restart_without_any_python_server(tmp_path):
    from backend.workspace_native import catalog_call
    executable=os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api for the cold native startup gate')
    state=tmp_path/'state'
    (state/'data').mkdir(parents=True)
    (state/'.env').write_text('TRUSTED_NETWORKS=\n',encoding='utf-8')
    database=state/'data/meta.db'
    with sqlite3.connect(state/'data/les_meta.db') as db:
        db.execute('CREATE TABLE auth_keys(key_value TEXT,role TEXT,expires_at TEXT,is_active INTEGER)')
        db.executemany('INSERT INTO auth_keys VALUES(?,?,?,?)',[
            ('admin-cold-fixture','admin',None,1),('user-cold-fixture','user',None,1)])
    frontend=static_frontend(tmp_path)
    listen=free_port();upstream=free_port()
    while listen==upstream: upstream=free_port()
    command=[executable,'--listen',f'127.0.0.1:{listen}','--upstream',f'http://127.0.0.1:{upstream}',
        '--database',str(database),'--state',str(state),'--frontend-assets',str(frontend)]
    environment={**os.environ,'LES_ENV_PATH':str(state/'.env'),
                 'PYTHONHOME':str(tmp_path/'missing-python'),'PYTHONPATH':str(tmp_path/'missing-python')}
    log=(tmp_path/'native-server.log').open('wb')
    children=[]
    def start():
        child=subprocess.Popen(command,cwd=tmp_path,env=environment,stdout=log,stderr=subprocess.STDOUT,
                               creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        children.append(child)
        wait_for(lambda:client.get('/api/model-connections/templates').status_code==200)
        return child
    with httpx.Client(base_url=f'http://127.0.0.1:{listen}',headers={'X-API-Key':'admin-cold-fixture'},
                      trust_env=False,timeout=10) as client:
        try:
            child=start()
            assert client.get('/api/model-connections').json()=={
                'connections':[], 'bindings':{'answer':None,'embeddings':None,'local_fallback':None}}
            assert client.get('/api/model-connections',headers={'X-API-Key':'user-cold-fixture'}).status_code==403
            assert client.get('/api/model-connections',headers={'X-API-Key':''}).status_code==401
            assert client.get('/api/workspace/sessions').status_code==200
            created=client.post('/api/workspace/sessions',json={'title':'После перезапуска 日本語'}).raise_for_status().json()
            sid=created['session_id']
            key=catalog_call(database,'chat_history_begin',safe_fallback='',session_id=sid,question='Прочитай PDF',attachment_context='')
            reason={'code':'PDF_PAGE_INCOMPLETE','detail':'Страница PDF прочитана не полностью.'}
            catalog_call(database,'chat_history_checkpoint',id=key,event='error',error=reason)
            before=client.get('/api/chat/history',params={'session_id':sid}).raise_for_status().json()
            assert before[-1]['meta']['error']==reason
            assert client.get('/app/').status_code==200
            assert client.get('/api/light/native-auth-policy').status_code==404
            child.kill();child.wait(timeout=10)
            start()
            after=client.get('/api/chat/history',params={'session_id':sid}).raise_for_status().json()
            assert after==before
            assert client.get(f'/api/workspace/sessions/{sid}').json()['title']=='После перезапуска 日本語'
            assert client.get('/api/chat/sessions').json()[0]['session_id']==sid
            # The retained routes are still unavailable, never reported as native functionality.
            assert client.get('/unported-fixture-route').status_code==503
        finally:
            for child in reversed(children):
                if child.poll() is None:child.kill()
                child.wait(timeout=10)
            log.close()


def test_native_first_start_key_setup_and_key_only_restart_without_python(tmp_path):
    executable = os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api for native access bootstrap')
    state = tmp_path/'state'; (state/'data').mkdir(parents=True)
    database = state/'data/meta.db'
    listen, upstream = free_port(), free_port()
    while listen == upstream: upstream = free_port()
    command = [executable, '--listen', f'127.0.0.1:{listen}', '--upstream', f'http://127.0.0.1:{upstream}',
               '--database', str(database), '--state', str(state)]
    environment = {k:v for k,v in os.environ.items() if not k.startswith(('TRUSTED_', 'CORS_'))}
    environment.update(LES_ENV_PATH=str(state/'.env'), PYTHONHOME=str(tmp_path/'absent'), PYTHONPATH=str(tmp_path/'absent'))
    children = []
    with (tmp_path/'cold-access.log').open('wb') as log, httpx.Client(base_url=f'http://127.0.0.1:{listen}', trust_env=False) as client:
        def start():
            child = subprocess.Popen(command, cwd=tmp_path, env=environment, stdout=log, stderr=log,
                                     creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
            children.append(child)
            wait_for(lambda: client.get('/api/model-connections/templates').status_code in (200,401))
            return child
        try:
            child = start()
            assert not database.exists()
            assert client.get('/api/auth/keys').raise_for_status().json() == []
            key = 'cold-autonomous-user-fixture'
            assert client.post('/api/auth/keys', json={'key_value':key,'role':'user'}).status_code == 200
            assert client.get('/api/workspace/sessions', headers={'X-Forwarded-For':'192.0.2.1'}).status_code == 401
            child.kill(); child.wait(timeout=10)
            (state/'.env').write_text("export 'TRUSTED_NETWORKS'='' # ${comment is not a value}\nTRUSTED_PROXY_NETWORKS=''\n", encoding='utf-8')
            start()
            assert client.get('/api/workspace/sessions').status_code == 401
            assert client.get('/api/workspace/sessions', headers={'X-Les-Trusted-Network':'true','X-Forwarded-For':'127.0.0.1'}).status_code == 401
            assert client.get('/api/workspace/sessions', headers={'X-API-Key':key}).status_code == 200
            assert client.post('/api/projects', headers={'X-API-Key':key}, json={'name':'Denied'}).status_code == 403
            for path in ('native-auth-policy','native-authorization','native-admin-authorization'):
                assert client.get('/api/light/'+path).status_code == 404
            assert not (state/'native-auth-policy.json').exists()
        finally:
            for child in reversed(children):
                if child.poll() is None: child.kill()
                child.wait(timeout=10)


@pytest.mark.parametrize('policy', [b'{}',b'x'*16385,b'{"schema":"les.native-auth-policy.v1","compatible":false}'])
def test_invalid_native_startup_policy_never_starts_or_creates_database(tmp_path,policy):
    executable=os.getenv('LES_TEST_NATIVE_API')
    if not executable:pytest.skip('Build les-api for startup policy validation')
    file=tmp_path/'policy.json';file.write_bytes(policy)
    listen=free_port();upstream=free_port()
    while listen==upstream:upstream=free_port()
    result=subprocess.run([executable,'--listen',f'127.0.0.1:{listen}','--upstream',f'http://127.0.0.1:{upstream}',
        '--database',str(tmp_path/'never-created.db'),'--state',str(tmp_path),'--auth-policy-file',str(file)],
        cwd=tmp_path,capture_output=True,timeout=10)
    assert result.returncode==1 and b'native_api_configuration_rejected' in result.stderr
    assert not (tmp_path/'never-created.db').exists()
