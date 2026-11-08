"""The model runtime does not consult Python for secrets, role snapshots or capability gates."""
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime,timedelta,timezone
import json
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from threading import Thread
from types import SimpleNamespace
from threading import Barrier
import pytest
from test_light_native_api import gateway,wait_for,MODEL_BRIDGE_TOKEN,native_trust
from proxy.services.model_secret_service import EnvironmentSecretStore,ModelSecretError
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_contracts import (
    CapabilityName,CapabilityObservation,CapabilitySnapshot,CapabilityState,ConnectionLocality,ConnectionRole,
)


def test_native_secret_updates_preserve_unrelated_unicode_and_first_match(tmp_path):
    path=tmp_path/'Настройки 日本語 с пробелами.env'
    path.write_text('# Лес 日本語\nOTHER=keep\nOPENAI_API_KEY=first-fixture\nOPENAI_API_KEY=duplicate-fixture\n',encoding='utf-8')
    environment={}
    store=EnvironmentSecretStore(path,environ=environment)
    assert store.status(None)=='not_required'
    assert store.resolve('env:OPENAI_API_KEY').reveal()=='first-fixture'
    saved=store.replace('env:OPENAI_API_KEY','updated-fixture',actor='admin:test')
    assert saved.status=='configured' and saved.secret_ref=='env:OPENAI_API_KEY'
    assert 'updated-fixture' not in repr(saved)
    assert environment['OPENAI_API_KEY']=='updated-fixture'
    assert store.resolve('env:OPENAI_API_KEY').reveal()=='updated-fixture'
    text=path.read_text(encoding='utf-8')
    assert text.count('OPENAI_API_KEY=')==1 and '# Лес 日本語\nOTHER=keep\n' in text
    assert list(tmp_path.glob('*.tmp'))==[]
    before=path.read_bytes()
    for reference in ('env:PATH','file:any','env:LES_MODEL_CONNECTION_x_API_KEY'):
        with pytest.raises(ModelSecretError,match='SECRET_REF_NOT_ALLOWED'):store.replace(reference,'bad',actor='admin:test')
    for secret in ('','  ','key\nOTHER=changed','x'*4097):
        with pytest.raises(ModelSecretError,match='SECRET_VALUE_INVALID'):store.replace('env:OPENAI_API_KEY',secret,actor='admin:test')
    assert path.read_bytes()==before


def test_parallel_secret_updates_never_lose_another_key(tmp_path):
    path=tmp_path/'.env';path.write_text('OTHER=unchanged\n',encoding='utf-8');barrier=Barrier(8)
    def write(index):
        store=EnvironmentSecretStore(path,environ={});barrier.wait(timeout=10)
        return store.replace(f'env:LES_MODEL_CONNECTION_TEST_{index}_API_KEY',f'fixture-{index}',actor=f'admin:{index}')
    with ThreadPoolExecutor(max_workers=8) as pool:results=list(pool.map(write,range(8)))
    assert len(results)==8
    store=EnvironmentSecretStore(path,environ={})
    for i in range(8):assert store.resolve(f'env:LES_MODEL_CONNECTION_TEST_{i}_API_KEY').reveal()==f'fixture-{i}'
    assert 'OTHER=unchanged\n' in path.read_text(encoding='utf-8')


def test_effective_roles_are_native_and_remain_exact_when_a_head_changes(gateway):
    client,database,folder=gateway
    native_trust(folder, 'admin')
    client.headers.pop('X-API-Key')
    registry=ModelConnectionRegistry(database)
    first=registry.create_connection(display_name='Тестовая модель',base_url='http://127.0.0.1:11434/v1',
        model_id='fixture:9b',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=24000,
        secret_ref=None,extension_type='ollama',actor='admin:test')
    registry.bind_role(ConnectionRole.ANSWER,first.revision_id,expected_binding_revision=None,actor='admin:test')
    observed=datetime(2026,1,1,tzinfo=timezone.utc)
    snapshot=CapabilitySnapshot('cap:observed',first.revision_id,(
        CapabilityObservation(CapabilityName.CHAT_COMPLETIONS,CapabilityState.SUPPORTED,'probe',observed),
    ),observed,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'24576'})
    registry.save_capability_snapshot(snapshot,actor='admin:test')
    role=client.get('/api/model-connections/effective').raise_for_status()
    assert role.headers['x-les-handler']=='rust.model-registry'
    assert role.json()['roles']['answer']['input_token_limit']==24000
    assert client.post('/test-stop-upstream').status_code==200
    wait_for(lambda:client.get('/api/light/instance').status_code==503)
    revised=registry.revise_connection(first.connection_id,expected_revision_id=first.revision_id,
                                       actor='admin:test',model_id='unassigned:other')
    answer=client.get('/api/model-connections/effective').raise_for_status().json()['roles']['answer']
    assert answer['revision_id']==first.revision_id and answer['model_id']=='fixture:9b'
    registry.disable_connection(first.connection_id,expected_revision_id=revised.revision_id,actor='admin:test')
    blocked=client.get('/api/model-connections/effective').raise_for_status().json()['roles']['answer']
    assert blocked['code']=='CONNECTION_DISABLED' and blocked['status']=='blocked'
    assert blocked['revision_id']==first.revision_id
    assert 'secret_ref' not in json.dumps(blocked)


def test_canonical_inference_owns_role_secret_payload_and_stream_without_backend(gateway):
    client,database,folder=gateway
    seen=[]
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert self.path=='/v1/chat/completions'
            assert self.headers.get('Authorization')=='Bearer fixture-native-owned-key'
            seen.append(value)
            assert value['model']=='fixture:9b' and value['max_tokens']==256
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
            for event in [
                {'model':'fixture:9b','choices':[{'delta':{'content':'Нативный ответ 日本語'}}]},
                {'choices':[{'delta':{},'finish_reason':'stop'}]},
                {'choices':[],'usage':{'prompt_tokens':21,'completion_tokens':4,'total_tokens':25}},
            ]:self.wfile.write(('data: '+json.dumps(event,ensure_ascii=False)+'\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n')
    server=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        registry=ModelConnectionRegistry(database)
        revision=registry.create_connection(display_name='Native fixture',base_url=f'http://127.0.0.1:{server.server_port}/v1',
            model_id='fixture:9b',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=24000,
            secret_ref='env:OPENAI_API_KEY',extension_type=None,actor='admin:test')
        registry.bind_role(ConnectionRole.ANSWER,revision.revision_id,expected_binding_revision=None,actor='admin:test')
        observed=datetime(2026,1,1,tzinfo=timezone.utc)
        snapshot=CapabilitySnapshot('cap:inference',revision.revision_id,tuple(
            CapabilityObservation(capability,CapabilityState.SUPPORTED,'probe',observed)
            for capability in (CapabilityName.CHAT_COMPLETIONS,CapabilityName.STREAMING)),
            observed,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'24576'})
        registry.save_capability_snapshot(snapshot,actor='admin:test')
        state=folder/'state';state.mkdir(exist_ok=True)
        EnvironmentSecretStore(state/'.env',environ={}).replace('env:OPENAI_API_KEY','fixture-native-owned-key',actor='admin:test')
        assert client.post('/test-stop-upstream').status_code==200
        wait_for(lambda:client.get('/api/light/instance').status_code==503)
        path='/api/light/internal-inference'
        payload={'schema':'les.inference.v1','role':'answer','request':{
            'messages':[{'role':'user','content':'Прочитай страницу 日本語'}],'max_output_tokens':256,'model':'must-not-override-registry'}}
        assert client.post(path,json=payload).status_code==404
        headers={'X-Les-Model-Token':MODEL_BRIDGE_TOKEN}
        denied=client.post(path,json=payload,headers={**headers,'Origin':str(client.base_url).rstrip('/')})
        assert denied.status_code==404 and not seen
        response=client.post(path,json=payload,headers=headers).raise_for_status()
        assert response.headers['x-les-handler']=='rust.inference'
        assert response.headers['x-les-model-protocol']=='les.inference.v1'
        events=[json.loads(line) for line in response.iter_lines()]
        assert ''.join(event.get('text','') for event in events)=='Нативный ответ 日本語'
        assert events[-1]['finish']['model_id']=='fixture:9b'
        assert events[-1]['finish']['usage']['total_tokens']==25
        assert seen[0]['messages'][0]['content']=='Прочитай страницу 日本語'
        registry.disable_connection(revision.connection_id,expected_revision_id=revision.revision_id,actor='admin:test')
        blocked=client.post(path,json=payload,headers=headers).raise_for_status().json()
        assert blocked=={'error':'CONNECTION_DISABLED'} and len(seen)==1
    finally:
        server.shutdown();server.server_close();thread.join(timeout=3)


@pytest.mark.asyncio
async def test_existing_chat_transport_is_only_a_revision_client(gateway,monkeypatch):
    import httpx
    from proxy.services import local_model_http
    from proxy.services.openai_compatible_transport_service import OpenAICompatibleTransport,InferenceRequest
    client,database,folder=gateway
    observed=[]
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert self.headers.get('Authorization')=='Bearer fixture-owned-key'
            observed.append(value)
            self.send_response(200);self.end_headers()
            if value.get('stream'):
                self.wfile.write(b'data: {"model":"native-fixture","choices":[{"delta":{"content":"native"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n')
            else:
                self.wfile.write(b'{"model":"native-fixture","choices":[{"message":{"content":"native"},"finish_reason":"stop"}]}')
    server=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        registry=ModelConnectionRegistry(database)
        revision=registry.create_connection(display_name='Native SDK fixture',base_url=f'http://127.0.0.1:{server.server_port}/v1',
            model_id='native-fixture',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=24000,
            secret_ref='env:OPENAI_API_KEY',extension_type=None,actor='admin:test')
        observed_at=datetime(2026,1,1,tzinfo=timezone.utc)
        def capabilities(identifier,when,streaming):
            snapshot=CapabilitySnapshot(identifier,revision.revision_id,tuple(
                CapabilityObservation(capability,CapabilityState.SUPPORTED,'probe',when)
                for capability in ([CapabilityName.CHAT_COMPLETIONS,CapabilityName.STREAMING] if streaming else [CapabilityName.CHAT_COMPLETIONS])),
                when,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'24576'})
            registry.save_capability_snapshot(snapshot,actor='admin:test')
        capabilities('cap:sdk:1',observed_at,True)
        (folder/'state').mkdir(exist_ok=True)
        EnvironmentSecretStore(folder/'state/.env',environ={}).replace('env:OPENAI_API_KEY','fixture-owned-key',actor='admin:test')
        monkeypatch.setattr(local_model_http,'_inference',(str(client.base_url.join('/api/light/internal-inference')),MODEL_BRIDGE_TOKEN))
        assert client.post('/test-stop-upstream').status_code==200
        wait_for(lambda:client.get('/api/light/instance').status_code==503)
        def forbidden_http(request):raise AssertionError('Python provider transport was used')
        class NoPythonSecrets:
            def resolve(self,*args):raise AssertionError('Python resolved a provider credential')
        # No endpoint, capabilities or preset exists on this object: Rust resolves them itself.
        connection=SimpleNamespace(revision_id=revision.revision_id,model_id='native-fixture',locality=ConnectionLocality.LOOPBACK)
        async with httpx.AsyncClient(transport=httpx.MockTransport(forbidden_http)) as http:
            transport=OpenAICompatibleTransport(client=http,secret_store=NoPythonSecrets())
            request=InferenceRequest(messages=[{'role':'user','content':'Проверь 日本語'}],max_output_tokens=256)
            events=[]
            async def sink(event):events.append(event)
            result=await transport.complete(connection,request,token_sink=sink)
            assert result.text=='native' and result.model_id=='native-fixture'
            assert events==[{'event':'token','data':'native'}]
            result=await transport.complete(connection,request)
            assert result.text=='native' and 'stream' not in observed[-1]
            capabilities('cap:sdk:2',observed_at+timedelta(minutes=1),False)
            events.clear()
            result=await transport.complete(connection,request,token_sink=sink)
            assert result.text=='native' and events[0]['event']=='progress'
            assert 'поток не поддерживается' in events[0]['data']['label']
            assert events[-1]=={'event':'token','data':'native'}
    finally:
        server.shutdown();server.server_close();thread.join(timeout=3)
