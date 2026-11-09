"""The model settings write/probe cycle without an application server."""
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from threading import Thread
import json

import pytest

from tests.test_light_native_chat_turn import native_turn

pytestmark=pytest.mark.parametrize('native_turn',['public'],indirect=True)


def test_native_settings_create_probe_assign_rotate_and_disable(native_turn):
    seen=[]

    class Probe(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            seen.append((self.path,self.headers.get('Authorization'),data))
            if self.path.endswith('/embeddings'):
                payload={'data':[{'embedding':[1.0,0.0]}]}
            else:
                payload={'choices':[{'message':{'content':'ok'}}]}
            body=json.dumps(payload).encode()
            self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers()
            self.wfile.write(body)

    service=ThreadingHTTPServer(('127.0.0.1',0),Probe)
    thread=Thread(target=service.serve_forever,daemon=True);thread.start()
    client=native_turn.client
    route='/api/model-connections'
    try:
        first=client.post(route,json={'display_name':'Локальная модель проверки','model_id':'native-fixture',
            'base_url':f'http://127.0.0.1:{service.server_port}/v1','locality':'loopback','secret_value':'fixture-key'})
        assert first.status_code==201 and first.headers['x-les-handler']=='rust.model-settings'
        row=first.json()
        assert row['secret_status']=='configured' and 'fixture-key' not in first.text and 'secret_ref' not in row
        ident=row['connection_id'];revision=row['revision_id']
        bind={'connection_revision_id':revision,'expected_binding_revision':None}
        assert client.put(route+'/roles/embeddings',json=bind).status_code==422
        probe=client.post(f'{route}/{ident}/test',json={'revision_id':revision,'capabilities':['embeddings','chat_completions']})
        assert probe.is_success,probe.json()
        assert {r['name'] for r in probe.json()['capabilities'] if r['state']=='supported'}=={'embeddings','chat_completions'}
        assert seen and all(row[1]=='Bearer fixture-key' for row in seen)
        binding=client.put(route+'/roles/embeddings',json=bind).raise_for_status().json()
        assert binding['connection_revision_id']==revision
        assert client.put(route+'/roles/embeddings',json=bind).status_code==409
        assert client.post(f'{route}/{ident}/secret',json={'expected_revision_id':'stale','secret_value':'wrong'}).status_code==409
        changed=client.post(f'{route}/{ident}/secret',json={'expected_revision_id':revision,'secret_value':'fixture-rotated'})
        assert changed.is_success and 'fixture-rotated' not in changed.text
        client.post(f'{route}/{ident}/test',json={'revision_id':revision,'capabilities':['embeddings']}).raise_for_status()
        assert seen[-1][1]=='Bearer fixture-rotated'
        redirected=client.post(f'{route}/{ident}/revisions',json={'expected_revision_id':revision,
            'base_url':f'http://127.0.0.1:{service.server_port}/different/v1'})
        assert redirected.status_code==409
        renamed=client.post(f'{route}/{ident}/revisions',json={'expected_revision_id':revision,'display_name':'Новое имя'}).raise_for_status().json()
        disable={'expected_revision_id':renamed['revision_id']}
        assert client.post(f'{route}/{ident}/disable',json=disable).status_code==409
        disabled=client.post(f'{route}/{ident}/disable',json={**disable,'confirm_bound_roles':True}).raise_for_status().json()
        assert disabled['enabled'] is False
        effective=client.get(route+'/effective').raise_for_status().json()['roles']['embeddings']
        assert effective['status']=='blocked' and effective['code']=='CONNECTION_DISABLED'
    finally:
        service.shutdown();service.server_close();thread.join(timeout=5)
