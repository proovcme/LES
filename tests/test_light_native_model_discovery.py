"""Read a model catalogue through the Rust handler, without saving a connection."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
import json

import pytest

from tests.test_light_native_chat_turn import native_turn

pytestmark=pytest.mark.parametrize('native_turn',['public'],indirect=True)


def test_native_discovery_pins_endpoint_and_never_follows_redirect(native_turn):
    seen=[]

    class Models(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_GET(self):
            seen.append((self.path,self.headers.get('Authorization')))
            if self.path.startswith('/redirect/'):
                self.send_response(302);self.send_header('Location','/never');self.end_headers()
                return
            body=json.dumps({'data':[{'id':name} for name in ['модель','a','a','']]}).encode()
            self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers()
            self.wfile.write(body)

    server=ThreadingHTTPServer(('127.0.0.1',0),Models)
    thread=Thread(target=server.serve_forever,daemon=True);thread.start()
    base=f'http://127.0.0.1:{server.server_port}'
    client=native_turn.client
    route='/api/model-connections/discover-models'
    before=client.get('/api/model-connections').raise_for_status().json()
    try:
        success=client.post(route,json={'base_url':base+'/custom/v1','secret_value':'fixture-only'})
        assert success.headers['x-les-handler']=='rust.model-discovery'
        assert success.json()=={'status':'ok','models':['a','модель'],'truncated':False}
        denied=client.post(route,json={'base_url':base+'/redirect','secret_value':'fixture-only'})
        assert denied.json()['status']=='unavailable' and 'fixture-only' not in denied.text
        assert seen==[('/custom/v1/models','Bearer fixture-only'),('/redirect/v1/models','Bearer fixture-only')]
        unsafe=client.post(route,json={'base_url':base,'locality':'remote'})
        assert unsafe.json()['status']=='unavailable' and len(seen)==2
        assert client.post(route,json={'base_url':base,'extra':1}).status_code==422
        local=client.post('/api/model-connections/discover-local')
        assert local.headers['x-les-handler']=='rust.model-discovery'
        assert local.json()['status']=='ok' and isinstance(local.json()['engines'],list)
        assert client.get('/api/model-connections').raise_for_status().json()==before
    finally:
        server.shutdown();server.server_close();thread.join(timeout=5)
