"""Native MCP settings and stateful SSE requests do not need the compatibility backend/SDK."""
import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
import threading

import pytest
from test_light_native_chat_profiles import profiles
from test_light_mcp_connections import server


def test_native_settings_and_cached_tool_revocation_without_backend(profiles, server, monkeypatch):
    client,database,_=profiles
    from proxy.services import mcp_connection_service as service
    from proxy.services.tool_harness_service import ToolHarness
    monkeypatch.setattr(service,'rag_meta_db_path',lambda:database)
    monkeypatch.setenv('LES_NATIVE_MCP_WORKER',os.environ['LES_TEST_NATIVE_API'])
    created=client.post('/api/mcp',json={'name':'Native','url':server[0]}).raise_for_status()
    assert created.headers['x-les-handler']=='rust.mcp'
    connection=created.json(); key=connection['id']
    assert client.get('/api/mcp').raise_for_status().json()['connections']==service.connections()
    assert len(client.post(f'/api/mcp/{key}/probe').raise_for_status().json()['tools'])==2
    assert client.put(f'/api/mcp/{key}/tools',json={'names':['change']}).status_code==409
    client.put(f'/api/mcp/{key}/tools',json={'names':['echo']}).raise_for_status()
    name=service.tool_name(key,'echo'); harness=ToolHarness()
    result=asyncio.run(harness.call_async(name,{'text':'Native read'}))
    assert result['status']=='ok',result
    assert server[1]==['Native read']
    renamed=client.put(f'/api/mcp/{key}',json={'name':'Renamed','url':server[0]}).raise_for_status().json()
    assert renamed['id']==key and renamed['name']=='Renamed'
    assert [tool['name'] for tool in renamed['tools']]==['echo']
    moved=client.put(f'/api/mcp/{key}',json={'name':'Renamed','url':server[0]+'/new'}).raise_for_status().json()
    assert moved['id']==key and moved['tools']==[]
    denied=asyncio.run(harness.call_async(name,{'text':'old tool must stay disabled'}))
    assert denied['status']!='ok' and server[1]==['Native read']
    client.delete(f'/api/mcp/{key}').raise_for_status()
    denied=asyncio.run(harness.call_async(name,{'text':'must not reach server'}))
    assert denied['status']!='ok'
    assert server[1]==['Native read']
    assert client.get('/api/mcp').json()=={'connections':[]}


@pytest.fixture
def sse_server():
    state={'calls':[],'closed':0,'wrong_id':False,'bad_schema':False,'seen':[],'pings':0,'denied_roots':0}
    class Handler(BaseHTTPRequestHandler):
        protocol_version='HTTP/1.1'
        def log_message(self,*args):pass
        def reply(self,body,status=200,sse=False,session=False):
            if sse:
                encoded=('event: message\r\ndata: '+json.dumps(body,ensure_ascii=False)+'\r\n\r\n').encode('utf-8')
                notifications=[{'jsonrpc':'2.0','id':'server-ping','method':'ping'},
                    {'jsonrpc':'2.0','id':'server-roots','method':'roots/list'}]
                encoded=''.join('data: '+json.dumps(item)+'\r\n\r\n' for item in notifications).encode()+encoded
            else: encoded=json.dumps(body,ensure_ascii=False).encode('utf-8') if body is not None else b''
            self.send_response(status);self.send_header('Content-Type','text/event-stream' if sse else 'application/json')
            self.send_header('Content-Length',str(len(encoded)))
            if session:self.send_header('Mcp-Session-Id','native-session')
            self.end_headers()
            for offset in range(0,len(encoded),3):self.wfile.write(encoded[offset:offset+3]);self.wfile.flush()
        def do_DELETE(self):
            assert self.headers['Mcp-Session-Id']=='native-session'
            state['closed']+=1;self.reply(None,204)
        def do_POST(self):
            request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            if 'method' not in request:
                assert self.headers['Mcp-Session-Id']=='native-session'
                if request['id']=='server-ping':assert request['result']=={};state['pings']+=1
                elif request['id']=='server-roots':assert request['error']['code']==-32601;state['denied_roots']+=1
                else:raise AssertionError(request)
                self.reply(None,202);return
            method=request['method'];state['seen'].append(method)
            assert 'application/json' in self.headers['Accept'] and 'text/event-stream' in self.headers['Accept']
            if method=='initialize':
                self.reply({'jsonrpc':'2.0','id':request['id'],'result':{'protocolVersion':'2025-06-18',
                    'capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'},
                    'instructions':'server-instruction-must-not-enter-profile'}},session=True);return
            assert self.headers['Mcp-Session-Id']=='native-session'
            assert self.headers['MCP-Protocol-Version']=='2025-06-18'
            if method=='notifications/initialized':self.reply(None,202);return
            if method=='tools/list':
                schema={'type':'object','properties':{'text':{'type':'string'}},'required':['text']}
                if state['bad_schema']:schema['properties']['text']={'$ref':'https://example.org/forbidden.json'}
                result={'tools':[{'name':'echo','inputSchema':schema,'annotations':{'readOnlyHint':True}}]}
                identifier=999 if state['wrong_id'] else request['id']
            elif method=='tools/call':
                state['calls'].append(request['params']['arguments']['text'])
                result={'content':[{'type':'text','text':'Ответ Я字: '+state['calls'][-1]}]};identifier=request['id']
            else:raise AssertionError(method)
            self.reply({'jsonrpc':'2.0','id':identifier,'result':result},sse=True)
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:yield f'http://127.0.0.1:{server.server_port}/mcp',state
    finally:server.shutdown();server.server_close();thread.join(timeout=5)


def test_stateful_sse_preserves_ids_and_utf8_denies_changed_schema_and_ignores_server_instructions(profiles,sse_server,monkeypatch):
    client,database,_=profiles
    from proxy.services import mcp_connection_service as service
    monkeypatch.setattr(service,'rag_meta_db_path',lambda:database)
    monkeypatch.setenv('LES_NATIVE_MCP_WORKER',os.environ['LES_TEST_NATIVE_API'])
    url,state=sse_server
    item=client.post('/api/mcp',json={'name':'SSE','url':url}).raise_for_status().json();key=item['id']
    enabled=client.put(f'/api/mcp/{key}/tools',json={'names':['echo']}).raise_for_status().json()
    expected=enabled['tools'][0]
    assert 'server-instruction' not in json.dumps(enabled)
    result=service._native('call',connection_id=key,expected=expected,arguments={'text':'Кедр'})
    assert result['content'][0]['text']=='Ответ Я字: Кедр'
    assert state['calls']==['Кедр'] and state['closed']>=2 and state['pings']>=2 and state['denied_roots']>=2
    state['wrong_id']=True
    with pytest.raises(ValueError,match='MCP_RESPONSE_ID_MISMATCH'):
        asyncio.run(service.discover(key))
    assert state['calls']==['Кедр']
    state['wrong_id']=False;state['bad_schema']=True
    assert client.put(f'/api/mcp/{key}/tools',json={'names':['echo']}).status_code==409
    with pytest.raises(ValueError,match='изменилось'):
        service._native('call',connection_id=key,expected=expected,arguments={'text':'must not execute'})
    assert state['calls']==['Кедр']
