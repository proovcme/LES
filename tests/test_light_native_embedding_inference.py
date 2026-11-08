"""Pinned registry, credentials, payload and embedding execution remain native with backend stopped."""
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from threading import Thread

import pytest
from test_light_native_api import gateway, wait_for, MODEL_BRIDGE_TOKEN
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_contracts import CapabilityName, CapabilityObservation, CapabilitySnapshot, CapabilityState, ConnectionLocality
from proxy.services.model_secret_service import EnvironmentSecretStore
from proxy.services import local_model_http


@pytest.mark.parametrize('native',[False,True])
def test_embedding_registry_credentials_protocol_and_failure_without_backend(gateway,native):
    client,database,folder=gateway
    seen=[]
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_POST(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert self.headers.get('Authorization')=='Bearer fixture-embedding-secret'
            seen.append((self.path,value))
            self.send_response(200); self.send_header('Content-Type','application/json'); self.end_headers()
            self.wfile.write(json.dumps({'model':'fixture-embed', 'embeddings':[[.25,.75]]} if native else {
                'model':'fixture-embed','data':[{'index':0,'embedding':[.25,.75]}]}).encode())
    server=ThreadingHTTPServer(('127.0.0.1',0),Model)
    thread=Thread(target=server.serve_forever,daemon=True); thread.start()
    try:
        registry=ModelConnectionRegistry(database)
        revision=registry.create_connection(display_name='Native embedding',base_url=f'http://127.0.0.1:{server.server_port}/v1',
            model_id='fixture-embed',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=8192,
            secret_ref='env:OPENAI_API_KEY',extension_type=None,actor='admin:test')
        observed=datetime(2026,1,1,tzinfo=timezone.utc)
        registry.save_capability_snapshot(CapabilitySnapshot('cap:embed',revision.revision_id,(
            CapabilityObservation(CapabilityName.EMBEDDINGS,CapabilityState.SUPPORTED,'probe',observed),),
            observed,datetime(2099,1,1,tzinfo=timezone.utc),
            {'observed_context_tokens':'8192',**({'embedding_protocol':'native_embedding_v1'} if native else {})}),actor='admin:test')
        state=folder/'state'; state.mkdir(exist_ok=True)
        EnvironmentSecretStore(state/'.env',environ={}).replace('env:OPENAI_API_KEY','fixture-embedding-secret',actor='admin:test')
        assert client.post('/test-stop-upstream').status_code==200
        wait_for(lambda:client.get('/api/light/instance').status_code==503)
        path='/api/light/internal-inference/embeddings'
        body={'schema':'les.embedding-inference.v1','revision_id':revision.revision_id,'inputs':['Кабель 日本語'],
              'timeout_seconds':30,'release_after':True,'model':'must-not-override-registry'}
        headers={'X-Les-Model-Token':MODEL_BRIDGE_TOKEN}
        assert client.post(path,json=body).status_code==404
        assert client.post(path,json=body,headers={**headers,'Origin':str(client.base_url).rstrip('/')}).status_code==404
        assert not seen
        response=client.post(path,json=body,headers=headers).raise_for_status()
        assert response.headers['x-les-handler']=='rust.embedding-inference'
        metadata,vectors=local_model_http.decode_embeddings(response.content,1)
        assert metadata['model_id']=='fixture-embed' and vectors==((.25,.75),)
        assert seen[0][0]==('/api/embed' if native else '/v1/embeddings')
        assert seen[0][1]['model']=='fixture-embed' and seen[0][1]['input']==['Кабель 日本語']
        if native:
            assert seen[0][1]['truncate'] is False and seen[0][1]['keep_alive']==0
            assert seen[0][1]['options']['num_ctx']==8192
        else:
            assert set(seen[0][1])=={'model','input'}
        registry.disable_connection(revision.connection_id,expected_revision_id=revision.revision_id,actor='admin:test')
        blocked=client.post(path,json=body,headers=headers)
        assert blocked.status_code==502 and blocked.json()['detail']=='CONNECTION_DISABLED'
        assert len(seen)==1
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=3)
