from __future__ import annotations

import asyncio
from dataclasses import replace
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import inspect
import json
import os
from pathlib import Path
import struct
from threading import Event, Thread

import httpx
import pytest
from test_light_native_api import MODEL_BRIDGE_TOKEN, gateway

from proxy.services import local_model_http

from proxy.services.model_connection_contracts import (
    CapabilityName,
    CapabilityObservation,
    CapabilitySnapshot,
    CapabilityState,
    ConnectionLocality,
)
from proxy.services.model_connection_resolver_service import ResolvedModelConnection
from proxy.services.model_connection_security_service import ValidatedEndpoint, validate_endpoint
from proxy.services.model_execution_preset_service import (
    BackendCapacity,
    resolve_execution_preset,
)
from proxy.services.model_secret_service import EnvironmentSecretStore
from proxy.services.openai_compatible_transport_service import (
    InferenceRequest,
    ModelTransportError,
    OpenAICompatibleTransport,
    read_chat_stream,
)


NOW = datetime(2026, 8, 28, tzinfo=timezone.utc)


@pytest.mark.asyncio
@pytest.mark.parametrize('ending',['complete','interrupt','cancel'])
@pytest.mark.parametrize('authenticated',[False,True])
async def test_openai_native_stream_delivers_before_finish_and_keeps_tools(gateway,monkeypatch,ending,authenticated):
    browser,_,folder=gateway
    bridge=str(browser.base_url.join('/api/light/internal-model-http'))
    monkeypatch.setattr(local_model_http,'_bridge',(bridge,MODEL_BRIDGE_TOKEN))
    if authenticated:
        monkeypatch.setattr(local_model_http,'getproxies',lambda:{})
        for key in ('SSL_CERT_FILE','SSL_CERT_DIR','CURL_CA_BUNDLE'):monkeypatch.delenv(key,raising=False)
    release=Event();disconnected=Event();observed=[]
    argument='{"page":6,"ref":123456789012345678901234567890}'

    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            payload=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert self.headers.get('Authorization')==('Bearer fixture-native-model-key' if authenticated else None)
            observed.append(payload)
            self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
            frame='data: '+json.dumps({'model':'model-1','choices':[{'delta':{'content':'Лес 日本語 😀',
                'tool_calls':[{'index':0,'id':'call-1','type':'function','function':{'name':'read_','arguments':argument[:10]}}]}}]},ensure_ascii=False)+'\r\n\r\n'
            try:
                for byte in frame.encode('utf-8'):self.wfile.write(bytes([byte]))
                self.wfile.flush()
                if ending=='cancel':
                    while not release.wait(.03):
                        self.wfile.write(b':still-running\n\n');self.wfile.flush()
                else:
                    release.wait(5)
                    if ending=='complete':
                        for payload in [
                            {'choices':[{'delta':{'tool_calls':[{'index':0,'function':{'name':'pdf_source','arguments':argument[10:]}}]},'finish_reason':'tool_calls'}]},
                            {'choices':[],'usage':{'prompt_tokens':21,'completion_tokens':7,'total_tokens':28}},
                        ]:self.wfile.write(('data: '+json.dumps(payload)+'\n\n').encode())
                        self.wfile.write(b'data: [DONE]\n\n');self.wfile.flush()
            except (BrokenPipeError,ConnectionResetError,ConnectionAbortedError):disconnected.set()

    server=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=server.serve_forever,daemon=True);thread.start()
    target=f'http://127.0.0.1:{server.server_port}/v1'
    endpoint=validate_endpoint(target,ConnectionLocality.LOOPBACK)
    connection=replace(_resolved(secret_ref='env:OPENAI_API_KEY' if authenticated else None),base_url=target,endpoint=endpoint)
    def forbidden_python_transport(request):raise AssertionError('The Python inference HTTP client was used')
    client=httpx.AsyncClient(transport=httpx.MockTransport(forbidden_python_transport))
    transport=OpenAICompatibleTransport(client=client,secret_store=EnvironmentSecretStore(folder/'.env',environ={'OPENAI_API_KEY':'fixture-native-model-key'}))
    first=asyncio.Event();events=[]
    async def sink(event):
        events.append(event)
        if event['event']=='token':first.set()
    task=asyncio.create_task(transport.complete(connection,InferenceRequest(
        messages=({'role':'user','content':'Проверь страницу'},),max_output_tokens=1024),token_sink=sink))
    try:
        await asyncio.wait_for(first.wait(),5)
        assert not task.done()
        assert events==[{'event':'token','data':'Лес 日本語 😀'}]
        if ending=='cancel':
            task.cancel()
            with pytest.raises(asyncio.CancelledError):await task
            assert await asyncio.to_thread(disconnected.wait,3)
        else:
            release.set()
            if ending=='interrupt':
                with pytest.raises(ModelTransportError,match='UPSTREAM_STREAM_INTERRUPTED'):await task
            else:
                answer=await task
                assert answer.text=='Лес 日本語 😀' and answer.finish_reason=='tool_calls'
                assert answer.tool_calls[0]['function']=={'name':'read_pdf_source','arguments':argument}
                assert answer.usage=={'prompt_tokens':21,'completion_tokens':7,'total_tokens':28}
        assert observed[0]['stream'] is True
    finally:
        release.set();task.cancel();await asyncio.gather(task,return_exceptions=True);await client.aclose()
        server.shutdown();server.server_close();thread.join(timeout=3)


@pytest.mark.asyncio
async def test_openai_native_nonstream_decode_and_errors(gateway,monkeypatch):
    browser,_,folder=gateway
    monkeypatch.setattr(local_model_http,'_bridge',(str(browser.base_url.join('/api/light/internal-model-http')),MODEL_BRIDGE_TOKEN))
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            scenario=value['messages'][-1]['content']
            if scenario=='redirect':self.send_response(307);self.send_header('Location','/other');self.end_headers();return
            if scenario=='missing':self.send_response(404);self.end_headers();return
            payload={'model':'model-1','choices':[{'message':{'content':'<think>Не показывать</think>Точный ответ',
                'tool_calls':[{'id':'1','type':'function','function':{'name':'read_pdf_source','arguments':'{"page":6}'}}]},'finish_reason':'tool_calls'}],
                'usage':{'prompt_tokens':19,'completion_tokens':8,'total_tokens':27}}
            if scenario=='exhausted':payload['choices'][0]={'message':{'content':'<think>unfinished'},'finish_reason':'length'}
            raw=b'broken-json' if scenario=='invalid' else json.dumps(payload,ensure_ascii=False).encode()
            self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
    server=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=server.serve_forever,daemon=True);thread.start()
    target=f'http://127.0.0.1:{server.server_port}/v1'
    endpoint=validate_endpoint(target,ConnectionLocality.LOOPBACK)
    connection=replace(_resolved(secret_ref=None),base_url=target,endpoint=endpoint)
    def forbidden_python_transport(request):raise AssertionError('Python HTTP transport used')
    async with httpx.AsyncClient(transport=httpx.MockTransport(forbidden_python_transport)) as client:
        transport=OpenAICompatibleTransport(client=client,secret_store=EnvironmentSecretStore(folder/'.env',environ={}))
        async def infer(text):return await transport.complete(connection,InferenceRequest(
            messages=({'role':'user','content':text},),max_output_tokens=1024,reasoning_enabled=True))
        try:
            answer=await infer('read')
            assert answer.text=='Точный ответ' and answer.model_id=='model-1'
            assert answer.tool_calls[0]['function']['arguments']=='{"page":6}'
            for scenario,code in [('exhausted','REASONING_BUDGET_EXHAUSTED'),('invalid','UPSTREAM_RESPONSE_INVALID'),
                                  ('redirect','UPSTREAM_REDIRECT_REJECTED'),('missing','UPSTREAM_HTTP_ERROR: 404')]:
                with pytest.raises(ModelTransportError,match=code):await infer(scenario)
        finally:
            server.shutdown();server.server_close();thread.join(timeout=3)


def test_native_authenticated_embeddings_pin_the_destination_and_keep_unicode(gateway):
    curl = Path(os.environ.get('SystemRoot', 'C:/Windows')) / 'System32/curl.exe' if os.name == 'nt' else Path('/usr/bin/curl')
    if not curl.is_file():
        pytest.skip('System TLS client unavailable')
    browser, _, _ = gateway
    seen = []

    class Embedding(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            seen.append((payload, self.headers.get('Authorization')))
            if payload.get('model') == 'redirect':
                self.send_response(302)
                self.send_header('Location', '/v1/embeddings')
                self.end_headers()
                return
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"model":"fixture","data":[{"index":0,"embedding":[0.1,-0.0]}]}')

    server = ThreadingHTTPServer(('127.0.0.1', 0), Embedding)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        target = f'http://localhost:{server.server_port}/v1/embeddings'
        headers = {'x-les-model-token': MODEL_BRIDGE_TOKEN, 'x-les-model-target': target,
            'x-les-embed-limits': '1/1024', 'x-les-embed-locality': 'loopback',
            'x-les-embed-addresses': '127.0.0.1', 'x-les-embed-deadline': '4',
            'Authorization': 'Bearer fixture-key'}
        route = '/api/light/internal-model-http'
        inputs = {'model': 'fixture', 'input': ['Кабель 東京 🌲 "точный" \\ тест\nСледующая строка']}
        answer = browser.post(route, headers=headers, json=inputs).raise_for_status()
        assert answer.headers['x-les-model-protocol'] == local_model_http.EMBED_PROTOCOL
        _, vectors = local_model_http.decode_embeddings(answer.content, 1)
        assert struct.pack('<2d', *vectors[0]) == struct.pack('<2d', 0.1, -0.0)
        assert seen == [(inputs, 'Bearer fixture-key')]
        redirect = browser.post(route, headers=headers, json={**inputs, 'model': 'redirect'})
        assert redirect.json()['detail'] == 'UPSTREAM_REDIRECT_REJECTED'
        bad_peer = browser.post(route, headers={**headers, 'x-les-embed-addresses': '169.254.169.254'}, json=inputs)
        assert bad_peer.status_code == 400 and len(seen) == 2
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


@pytest.mark.asyncio
async def test_local_generation_and_embeddings_use_rust_http(gateway, monkeypatch):
    browser, _, tmp_path = gateway
    bridge = str(browser.base_url.join('/api/light/internal-model-http'))
    monkeypatch.setattr(local_model_http, '_bridge', None)
    monkeypatch.setenv('LES_INTERNAL_MODEL_URL', bridge)
    monkeypatch.setenv('LES_INTERNAL_MODEL_TOKEN', MODEL_BRIDGE_TOKEN)
    local_model_http.initialize()
    assert 'LES_INTERNAL_MODEL_TOKEN' not in local_model_http.os.environ

    finish = Event()
    cancelled = Event()
    oversized = Event()
    calls = []
    big_argument = 123456789012345678901234567890
    precise_vector = (1.5, -0.0, float.fromhex('0x0.0000000000001p-1022'), 1.0000000000000002, 0.1, 1e30)

    class Model(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def write_frames(self, *frames):
            for frame in frames:
                self.wfile.write(json.dumps(frame, ensure_ascii=False).encode() + b'\r\n')

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            calls.append((self.path, request, dict(self.headers)))
            if self.path == '/v1/embeddings':
                second = 0 if request['input'][0] == 'duplicate index' else 1
                raw = json.dumps({'data': [{'index': second, 'embedding': [7]}, {'index': 0, 'embedding': [4]}]}).encode()
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)
                return
            if self.path == '/api/embed':
                scenario = request['input'][0]
                payload = {'model': request['model'], 'embeddings': [precise_vector], 'usage': {'total_tokens': 7}}
                if scenario == 'wrong dimension':
                    payload['embeddings'] = [[0.1], [0.1, 0.2]]
                if scenario == 'empty vector':
                    payload['embeddings'] = [[]]
                if scenario == 'invalid type':
                    payload['embeddings'] = [[True]]
                raw = json.dumps(payload).encode()
                if scenario == 'nonfinite':
                    raw = b'{"model":"model-1","embeddings":[[1e309]]}'
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                if scenario != 'oversized embedding':
                    self.send_header('Content-Length', str(len(raw)))
                self.end_headers()
                if scenario == 'oversized embedding':
                    self.wfile.write(raw + b' ' * 2048)
                    return
                self.wfile.write(raw)
                return

            text = request['messages'][0]['content']
            if text == 'redirect':
                self.send_response(307)
                self.send_header('Location', '/api/embed')
                self.end_headers()
                return
            if text == 'missing':
                self.send_response(404)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header('Content-Type', 'application/x-ndjson')
            self.end_headers()
            if text == 'reasoning':
                self.write_frames(
                    {'message': {'content': '<thi'}},
                    {'message': {'content': 'nk>Скрытое размышление</th'}},
                    {'message': {'content': 'ink>Точный ответ',
                                 'tool_calls': [{'function': {'name': 'read',
                                                              'arguments': {'ref': big_argument}}}]}},
                    {'message': {}, 'done': True, 'done_reason': 'tool_calls',
                     'prompt_eval_count': '11', 'eval_count': 4},
                )
                return
            if text == 'exhausted':
                self.write_frames({'message': {'thinking': 'internal'}, 'done': True, 'done_reason': 'length'})
                return
            if text == 'bad frame':
                self.write_frames([])
                return
            if text == 'oversized':
                self.wfile.write(b'{"message":{"content":"' + b'x' * 2048)
                oversized.wait(6)
                return
            first = '{"message":{"content":"Лес 🌲"},"done":false}\n'.encode()
            midpoint = first.index('🌲'.encode()) + 1
            self.wfile.write(first[:midpoint])
            self.wfile.write(first[midpoint:])
            if text == 'cut':
                return
            if text == 'cancel':
                try:
                    while not cancelled.wait(.02):
                        self.wfile.write(b'{"message":{"content":""},"done":false}\n')
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    cancelled.set()
                return
            if finish.wait(6):
                self.wfile.write(
                    b'{"message":{"content":"!"},"model":"model-1","done":true,'
                    b'"done_reason":"stop","prompt_eval_count":7,"eval_count":3}\n'
                )

    def direct_call(_request):
        pytest.fail('An eligible request bypassed the Rust bridge')

    server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        base = f'http://127.0.0.1:{server.server_port}/v1'
        endpoint = validate_endpoint(base, ConnectionLocality.LOOPBACK)
        connection = _resolved(chat_protocol='native_chat_v1')
        snapshot = replace(connection.capability_snapshot, transport_options={
            'chat_protocol': 'native_chat_v1', 'embedding_protocol': 'native_embedding_v1',
        })
        connection = replace(connection, endpoint=endpoint, base_url=base, capability_snapshot=snapshot)
        async with httpx.AsyncClient(transport=httpx.MockTransport(direct_call)) as client:
            transport = OpenAICompatibleTransport(
                client=client, secret_store=EnvironmentSecretStore(tmp_path / '.env'), timeout=4,
            )

            def infer(text, sink=None, reasoning=False):
                return transport.complete(connection, InferenceRequest(
                    messages=[{'role': 'user', 'content': text}], max_output_tokens=64,
                    reasoning_enabled=reasoning,
                ), token_sink=sink)

            tokens = []

            async def sink(event):
                if event['event'] == 'token':
                    tokens.append(event['data'])
                    finish.set()

            reply = await infer('Прочитай 日本語', sink)
            assert reply.text == 'Лес 🌲!' and tokens == ['Лес 🌲', '!']
            assert reply.model_id == 'model-1' and reply.usage['total_tokens'] == 10
            assert calls[0][1]['messages'][0]['content'] == 'Прочитай 日本語'
            assert calls[0][1]['model'] == 'model-1'
            assert all(key.lower() not in {'authorization', 'x-les-model-token', 'x-les-model-target', 'x-les-chat-limits'}
                       for key in calls[0][2])
            assert {key.lower(): value for key, value in calls[0][2].items()}['accept-encoding'] == 'identity'

            embedding = await transport.embed(connection, ['Пункт 3.2'], release_after=True)
            assert embedding.model_id == 'model-1' and embedding.usage['total_tokens'] == 7
            assert struct.pack('<6d', *embedding.vectors[0]) == struct.pack('<6d', *precise_vector)
            assert calls[1][1]['keep_alive'] == 0 and calls[1][1]['input'] == ['Пункт 3.2']
            embed_headers = {key.lower(): value for key, value in calls[1][2].items()}
            assert embed_headers['accept-encoding'] == 'identity' and 'x-les-embed-limits' not in embed_headers
            for inputs, code in (
                (['wrong count', 'extra input'], 'UPSTREAM_EMBEDDING_COUNT_MISMATCH'),
                (['wrong dimension', 'extra input'], 'UPSTREAM_EMBEDDING_RESPONSE_INVALID'),
                (['empty vector'], 'UPSTREAM_EMBEDDING_RESPONSE_INVALID'),
                (['invalid type'], 'UPSTREAM_EMBEDDING_RESPONSE_INVALID'),
                (['nonfinite'], 'UPSTREAM_EMBEDDING_RESPONSE_INVALID'),
                (['oversized embedding'], 'UPSTREAM_RESPONSE_TOO_LARGE'),
            ):
                transport.response_body_limit = 512
                with pytest.raises(ModelTransportError, match=code):
                    await transport.embed(connection, inputs)
            transport.response_body_limit = 32768
            compatible = replace(connection, capability_snapshot=replace(snapshot, transport_options={}))
            reply = await transport.embed(compatible, ['first input', 'second input'])
            assert reply.model_id == 'model-1' and reply.vectors == ((4.0,), (7.0,))
            with pytest.raises(ModelTransportError, match='UPSTREAM_EMBEDDING_RESPONSE_INVALID'):
                await transport.embed(compatible, ['duplicate index', 'second input'])
            for text, error in (
                ('redirect', 'UPSTREAM_REDIRECT_REJECTED'),
                ('missing', 'UPSTREAM_HTTP_ERROR: 404'),
                ('cut', 'UPSTREAM_STREAM_INTERRUPTED'),
                ('bad frame', 'UPSTREAM_RESPONSE_INVALID'),
                ('exhausted', 'REASONING_BUDGET_EXHAUSTED'),
            ):
                with pytest.raises(ModelTransportError, match=error):
                    await infer(text)

            events = []

            async def collect(event):
                events.append(event)

            answer = await infer('reasoning', collect, reasoning=True)
            assert [event['event'] for event in events] == ['progress', 'token']
            assert events[-1]['data'] == answer.text == 'Точный ответ'
            assert answer.finish_reason == 'tool_calls'
            assert answer.usage['total_tokens'] == 15
            assert answer.model_id == 'model-1'
            assert answer.tool_calls[0]['function']['arguments']['ref'] == big_argument

            transport.response_body_limit = 64
            try:
                with pytest.raises(ModelTransportError, match='UPSTREAM_RESPONSE_TOO_LARGE'):
                    await infer('oversized')
            finally:
                oversized.set()
            transport.response_body_limit = 32768

            first_token = asyncio.Event()

            async def observe(event):
                if event['event'] == 'token':
                    first_token.set()

            task = asyncio.create_task(infer('cancel', observe))
            try:
                await asyncio.wait_for(first_token.wait(), timeout=3)
            finally:
                task.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await task
            assert await asyncio.to_thread(cancelled.wait, 3)
            assert [path for path, _, _ in calls] == (
                ['/api/chat'] + ['/api/embed'] * 7 + ['/v1/embeddings'] * 2 + ['/api/chat'] * 8
            )
    finally:
        finish.set()
        cancelled.set()
        oversized.set()
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)


def test_native_image_transport_preserves_pixels_and_text():
    request = InferenceRequest(messages=[{"role": "user", "content": [
        {"type": "text", "text": "Describe the picture"},
        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AQID"}},
    ]}], max_output_tokens=200)
    body = OpenAICompatibleTransport._native_chat_body(_resolved(), request)
    assert body["messages"] == [{"role": "user", "content": "Describe the picture", "images": ["AQID"]}]
    assert isinstance(request.messages[0]["content"], list)


@pytest.mark.asyncio
@pytest.mark.parametrize('arguments', ['{"operation":"status","offset":0,"confirm":false}',
                                     {'operation': 'status', 'offset': 0, 'confirm': False}])
async def test_native_tool_continuation_sends_object_arguments_and_tool_name(tmp_path, arguments):
    messages = ({'role': 'assistant', 'content': None, 'tool_calls': [
        {'id': 'review_call', 'type': 'function', 'function': {'name': 'document_review', 'arguments': arguments}}]},
        {'role': 'tool', 'tool_call_id': 'review_call', 'name': 'document_review', 'content': '{"pending":1}'})
    before = json.dumps(messages, ensure_ascii=False)
    def handler(request):
        body = json.loads(request.content)
        assert body['messages'][0]['tool_calls'][0]['function']['arguments'] == {
            'operation': 'status', 'offset': 0, 'confirm': False}
        assert body['messages'][1]['tool_name'] == 'document_review'
        assert body['messages'][1]['tool_call_id'] == 'review_call'
        return httpx.Response(200, json={'message': {'content': 'Продолжаю'}, 'done': True, 'done_reason': 'stop'})
    transport, client = _transport(tmp_path, handler)
    try:
        result = await transport.complete(_resolved(chat_protocol='native_chat_v1'),
            InferenceRequest(messages=messages, max_output_tokens=32))
        assert result.text == 'Продолжаю'
    finally:
        await client.aclose()
    assert json.dumps(messages, ensure_ascii=False) == before


@pytest.mark.parametrize('arguments', ['{broken', '[1,2]', 'null', 2])
def test_native_tool_arguments_reject_invalid_or_nonobject_json(arguments):
    with pytest.raises(ModelTransportError, match='INVALID_NATIVE_TOOL_ARGUMENTS'):
        OpenAICompatibleTransport._native_chat_body(_resolved(), InferenceRequest(messages=(
            {'role': 'assistant', 'tool_calls': [{'function': {'name': 'read', 'arguments': arguments}}]},),
            max_output_tokens=1))


@pytest.mark.asyncio
async def test_openai_tool_continuation_keeps_argument_string(tmp_path):
    arguments = '{"operation":"status"}'
    def handler(request):
        body = json.loads(request.content)
        assert body['messages'][0]['tool_calls'][0]['function']['arguments'] == arguments
        assert 'tool_name' not in body['messages'][1]
        return httpx.Response(200, json={'choices': [{'message': {'content': 'ok'}, 'finish_reason': 'stop'}]})
    transport, client = _transport(tmp_path, handler)
    try:
        response = await transport.complete(_resolved(), InferenceRequest(messages=(
            {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'call', 'type': 'function',
                'function': {'name': 'document_review', 'arguments': arguments}}]},
            {'role': 'tool', 'name': 'document_review', 'tool_call_id': 'call', 'content': '{}'}), max_output_tokens=1))
        assert response.text == 'ok'
    finally:
        await client.aclose()


def _resolved(
    *,
    display_name: str = "Connection",
    secret_ref: str | None = None,
    max_output_field: str = "max_tokens",
    chat_protocol: str | None = None,
) -> ResolvedModelConnection:
    observations = tuple(
        CapabilityObservation(
            capability=capability,
            state=CapabilityState.SUPPORTED,
            evidence_source="probe",
            observed_at=NOW,
            detail="http_200",
        )
        for capability in (
            CapabilityName.CHAT_COMPLETIONS,
            CapabilityName.STREAMING,
            CapabilityName.TOOLS,
            CapabilityName.STRUCTURED_OUTPUT,
            CapabilityName.EMBEDDINGS,
        )
    )
    transport_options = {"max_output_field": max_output_field}
    if chat_protocol is not None:
        transport_options["chat_protocol"] = chat_protocol
    snapshot = CapabilitySnapshot(
        snapshot_id="cap:c1:r1",
        connection_revision_id="conn:c1:r1",
        observations=observations,
        observed_at=NOW,
        expires_at=NOW + timedelta(days=1),
        transport_options=transport_options,
    )
    endpoint = ValidatedEndpoint(
        canonical_base_url="http://127.0.0.1:1919/v1",
        locality=ConnectionLocality.LOOPBACK,
        host="127.0.0.1",
        port=1919,
        allowed_addresses=frozenset(),
    )
    return ResolvedModelConnection(
        connection_id="conn:c1",
        revision_id="conn:c1:r1",
        display_name=display_name,
        base_url=endpoint.canonical_base_url,
        model_id="model-1",
        locality=ConnectionLocality.LOOPBACK,
        requested_context_tokens=8192,
        secret_ref=secret_ref,
        extension_type=None,
        capability_snapshot=snapshot,
        endpoint=endpoint,
        effective_preset=resolve_execution_preset(
            BackendCapacity("connection", "model-1", 8192, True, "probe")
        ),
    )


def _transport(tmp_path, handler, *, environ=None, body_limit=32768, peer_verifier=None):
    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    return OpenAICompatibleTransport(
        client=client,
        secret_store=EnvironmentSecretStore(tmp_path / ".env", environ=environ or {}),
        peer_verifier=peer_verifier or (lambda _response, _endpoint: None),
        response_body_limit=body_limit,
    ), client


@pytest.mark.asyncio
@pytest.mark.parametrize("display_name", ["FreeToken", "Ollama", "Lemonade", "MLX", "Renamed"])
async def test_transport_behavior_does_not_depend_on_display_name(tmp_path, display_name):
    requests = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return httpx.Response(
            200,
            json={
                "model": "observed-model-1",
                "choices": [{"message": {"content": "Готово"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1},
            },
        )

    transport, client = _transport(tmp_path, handler)
    try:
        result = await transport.complete(
            _resolved(display_name=display_name),
            InferenceRequest(
                messages=({"role": "user", "content": "test"},),
                max_output_tokens=64,
            ),
        )
    finally:
        await client.aclose()

    assert result.text == "Готово"
    assert result.model_id == "observed-model-1"
    assert requests[0].url.path.endswith("/v1/chat/completions")


@pytest.mark.asyncio
async def test_complete_preserves_tool_calls_auth_and_output_field(tmp_path):
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["request"] = request
        captured["body"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "choices": [{
                    "message": {
                        "reasoning_content": "checked",
                        "tool_calls": [{
                            "id": "call-1",
                            "type": "function",
                            "function": {"name": "read_file", "arguments": "{\"id\":1}"},
                        }],
                    },
                    "finish_reason": "tool_calls",
                }],
                "usage": {"total_tokens": 9},
            },
        )

    transport, client = _transport(
        tmp_path,
        handler,
        environ={"OPENAI_API_KEY": "secret-value"},
    )
    try:
        result = await transport.complete(
            _resolved(
                secret_ref="env:OPENAI_API_KEY",
                max_output_field="max_completion_tokens",
            ),
            InferenceRequest(
                messages=({"role": "user", "content": "test"},),
                max_output_tokens=21,
                tools=({"type": "function", "function": {"name": "read_file"}},),
            ),
        )
    finally:
        await client.aclose()

    assert captured["request"].headers["Authorization"] == "Bearer secret-value"
    assert captured["body"]["max_completion_tokens"] == 21
    assert "max_tokens" not in captured["body"]
    assert result.text == ""  # Provider reasoning is not the user-facing answer.
    assert result.tool_calls[0]["function"]["name"] == "read_file"
    assert result.finish_reason == "tool_calls"


@pytest.mark.asyncio
async def test_complete_places_system_messages_before_conversation(tmp_path):
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={"choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]},
        )

    transport, client = _transport(tmp_path, handler)
    try:
        await transport.complete(
            _resolved(),
            InferenceRequest(
                messages=(
                    {"role": "user", "content": "question"},
                    {"role": "system", "content": "policy"},
                    {"role": "assistant", "content": "prior"},
                    {"role": "system", "content": "evidence rules"},
                ),
                max_output_tokens=32,
            ),
        )
    finally:
        await client.aclose()

    assert [item["role"] for item in captured["body"]["messages"]] == [
        "system",
        "user",
        "assistant",
    ]
    assert captured["body"]["messages"][0]["content"] == "policy\n\nevidence rules"


@pytest.mark.asyncio
async def test_complete_uses_capability_selected_native_chat_without_reasoning(tmp_path):
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["path"] = request.url.path
        captured["body"] = json.loads(request.content)
        return httpx.Response(
            200,
            json={
                "model": "observed-model-1",
                "message": {"content": "ЛЕС", "thinking": ""},
                "done_reason": "stop",
                "done": True,
                "prompt_eval_count": 10,
                "eval_count": 1,
            },
        )

    transport, client = _transport(tmp_path, handler)
    try:
        result = await transport.complete(
            _resolved(chat_protocol="native_chat_v1"),
            InferenceRequest(
                messages=({"role": "user", "content": "Ответь одним словом"},),
                max_output_tokens=64,
                temperature=0.0,
            ),
        )
    finally:
        await client.aclose()

    assert captured["path"] == "/api/chat"
    assert captured["body"]["think"] is False
    assert captured["body"]["options"] == {
        "num_predict": 64,
        "num_ctx": 8192,
        "temperature": 0.0,
    }
    assert result.text == "ЛЕС"
    assert result.finish_reason == "stop"
    assert result.usage == {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11}


@pytest.mark.asyncio
async def test_native_complete_consumes_incremental_events_into_unchanged_answer(tmp_path):
    captured = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        events = (
            {"model": "observed-model-1", "message": {"content": "Первая "}, "done": False},
            {"model": "observed-model-1", "message": {"content": "строка"}, "done": False},
            {
                "model": "observed-model-1",
                "message": {"content": ""},
                "done": True,
                "done_reason": "stop",
                "prompt_eval_count": 12,
                "eval_count": 2,
            },
        )
        return httpx.Response(
            200,
            content=b"".join(
                json.dumps(event, ensure_ascii=False).encode("utf-8") + b"\n"
                for event in events
            ),
            headers={"content-type": "application/x-ndjson"},
        )

    # The wire representation contains repeated per-event metadata and is much
    # larger than the model-authored text.  The configured response limit owns
    # semantic output, not native streaming protocol overhead.
    transport, client = _transport(tmp_path, handler, body_limit=64)
    try:
        result = await transport.complete(
            _resolved(chat_protocol="native_chat_v1"),
            InferenceRequest(
                messages=({"role": "user", "content": "Верни две части"},),
                max_output_tokens=64,
            ),
        )
    finally:
        await client.aclose()

    assert captured["body"]["stream"] is True
    assert result.text == "Первая строка"
    assert result.finish_reason == "stop"
    assert result.usage == {"prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14}


@pytest.mark.asyncio
async def test_complete_retries_without_system_role_for_restrictive_compatible_server(tmp_path):
    bodies = []

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        bodies.append(body)
        if any(item.get("role") == "system" for item in body["messages"]):
            return httpx.Response(502, json={"error": "System message must be at the beginning."})
        return httpx.Response(
            200,
            json={"choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]},
        )

    transport, client = _transport(tmp_path, handler)
    try:
        result = await transport.complete(
            _resolved(),
            InferenceRequest(
                messages=(
                    {"role": "system", "content": "policy"},
                    {"role": "user", "content": "question"},
                ),
                max_output_tokens=32,
            ),
        )
    finally:
        await client.aclose()

    assert result.text == "ok"
    assert len(bodies) == 2
    assert [item["role"] for item in bodies[1]["messages"]] == ["user"]
    assert bodies[1]["messages"][0]["content"] == "policy\n\nquestion"


@pytest.mark.asyncio
async def test_stream_normalizes_text_deltas_and_finish(tmp_path):
    payload = (
        'data: {"model":"observed-stream-model","choices":[{"delta":{"reasoning":"one"}}]}\n\n'
        'data: {"model":"observed-stream-model","choices":[{"delta":{"content":" two"},"finish_reason":"stop"}]}\n\n'
        "data: [DONE]\n\n"
    )

    def handler(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, text=payload, headers={"content-type": "text/event-stream"})

    transport, client = _transport(tmp_path, handler)
    try:
        events = [
            event
            async for event in transport.stream(
                _resolved(),
                InferenceRequest(
                    messages=({"role": "user", "content": "test"},),
                    max_output_tokens=10,
                ),
            )
        ]
    finally:
        await client.aclose()

    assert [event.text for event in events if event.kind == "text_delta"] == [" two"]
    assert [event.kind for event in events].count("reasoning") == 1
    assert events[-1].kind == "finish"
    assert events[-1].finish_reason == "stop"
    assert {event.model_id for event in events} == {"observed-stream-model"}


@pytest.mark.asyncio
async def test_chat_completion_preserves_interleaved_streamed_tools_and_late_usage(tmp_path):
    frames = [
        {"choices": [{"delta": {"reasoning": "служебное рассуждение"}}]},
        {"model": "fixture-chat", "choices": [{"delta": {"content": "Кабель 東京"}}]},
        {"choices": [{"delta": {"tool_calls": [
            {"index": 1, "id": "second", "function": {"name": "open_", "arguments": '{"page":'}},
            {"index": 0, "id": "first", "function": {"name": "read", "arguments": "{}"}},
        ]}}]},
        {"choices": [{"delta": {"tool_calls": [
            {"index": 1, "function": {"name": "file", "arguments": "2}"}},
        ]}, "finish_reason": "tool_calls"}]},
        {"choices": [], "usage": {"prompt_tokens": 11, "completion_tokens": 5}},
    ]
    payload = "".join(f"data: {json.dumps(frame, ensure_ascii=False)}\n\n" for frame in frames)
    transport, client = _transport(tmp_path, lambda _: httpx.Response(200, text=payload + "data: [DONE]\n\n"))
    shown = []

    async def sink(event):
        shown.append(event)

    try:
        result = await transport.complete(_resolved(), InferenceRequest(
            messages=({"role": "user", "content": "Открой документ"},), max_output_tokens=32,
        ), token_sink=sink)
    finally:
        await client.aclose()

    assert result.model_id == "fixture-chat"
    assert result.text == "Кабель 東京"
    assert result.finish_reason == "tool_calls"
    assert result.usage == {"prompt_tokens": 11, "completion_tokens": 5}
    assert [(call["id"], call["function"]) for call in result.tool_calls] == [
        ("first", {"name": "read", "arguments": "{}"}),
        ("second", {"name": "open_file", "arguments": '{"page":2}'}),
    ]
    assert [event["event"] for event in shown] == ["progress", "token"]
    assert shown[-1]["data"] == "Кабель 東京"


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", ["eof", "read_error", "connect_error"])
async def test_stream_disconnect_never_completes_partial_answer(tmp_path, failure):
    class BrokenStream(httpx.AsyncByteStream):
        async def __aiter__(self):
            yield b'data: {"choices":[{"delta":{"content":"partial"}}]}\n\n'
            if failure == "read_error":
                raise httpx.ReadError("connection lost")

    def handler(request):
        if failure == "connect_error":
            raise httpx.ConnectError("model offline", request=request)
        return httpx.Response(200, stream=BrokenStream())

    transport, client = _transport(tmp_path, handler)
    events = []
    try:
        with pytest.raises(ModelTransportError):
            async for event in transport.stream(_resolved(), InferenceRequest(messages=({"role": "user", "content": "test"},), max_output_tokens=10)):
                events.append(event)
    finally:
        await client.aclose()
    assert not any(event.kind == "finish" for event in events)
    if failure != "connect_error":
        assert [event.text for event in events] == ["partial"]


@pytest.mark.asyncio
async def test_embeddings_are_returned_in_server_index_order(tmp_path):
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/v1/embeddings")
        return httpx.Response(
            200,
            json={
                "model": "observed-model",
                "data": [
                    {"index": 1, "embedding": [2.0]},
                    {"index": 0, "embedding": [1.0]},
                ],
                "usage": {"prompt_tokens": 2},
            },
        )

    transport, client = _transport(tmp_path, handler)
    try:
        result = await transport.embed(_resolved(), ("first", "second"))
    finally:
        await client.aclose()

    assert result.vectors == ((1.0,), (2.0,))
    assert result.model_id == "observed-model"


@pytest.mark.asyncio
async def test_peer_is_checked_before_response_body_is_accepted(tmp_path):
    checked = []

    def handler(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}]})

    def reject(response, endpoint):
        checked.append((response.status_code, endpoint.host))
        raise ValueError("CONNECTED_PEER_MISMATCH")

    transport, client = _transport(tmp_path, handler, peer_verifier=reject)
    try:
        with pytest.raises(ModelTransportError, match="CONNECTED_PEER_MISMATCH"):
            await transport.complete(
                _resolved(),
                InferenceRequest(messages=({"role": "user", "content": "x"},), max_output_tokens=1),
            )
    finally:
        await client.aclose()
    assert checked == [(200, "127.0.0.1")]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("status", "body_limit", "expected"),
    ((302, 32768, "UPSTREAM_REDIRECT_REJECTED"), (500, 4, "UPSTREAM_RESPONSE_TOO_LARGE")),
)
async def test_redirects_and_oversized_error_bodies_fail_closed(tmp_path, status, body_limit, expected):
    def handler(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(status, content=b"abcdefgh")

    transport, client = _transport(tmp_path, handler, body_limit=body_limit)
    try:
        with pytest.raises(ModelTransportError, match=expected):
            await transport.complete(
                _resolved(),
                InferenceRequest(messages=({"role": "user", "content": "x"},), max_output_tokens=1),
            )
    finally:
        await client.aclose()


def test_transport_source_has_no_engine_name_branches() -> None:
    source = inspect.getsource(OpenAICompatibleTransport).lower()
    assert not any(name in source for name in ("freetoken", "ollama", "lemonade", "mlx"))


@pytest.mark.asyncio
@pytest.mark.parametrize("ending", ["eof", "read_error", "done", "finish_reason"])
async def test_chat_stream_requires_completion_and_preserves_tokens(ending):
    class Stream(httpx.AsyncByteStream):
        async def __aiter__(self):
            yield b'data: {"choices":[{"delta":{"content":"part"}}]}\n\n'
            if ending == "read_error":
                raise httpx.ReadError("lost connection")
            if ending == "done":
                yield b'data: [DONE]\n\n'
            if ending == "finish_reason":
                yield b'data: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":2}}\n\n'

    events = []

    async def sink(event):
        events.append(event)

    response = httpx.Response(200, request=httpx.Request("POST", "http://model.test/chat"), stream=Stream())
    try:
        if ending in {"eof", "read_error"}:
            with pytest.raises(ModelTransportError, match="UPSTREAM_STREAM_INTERRUPTED"):
                await read_chat_stream(response, sink)
        else:
            text, usage = await read_chat_stream(response, sink)
            assert text == "part"
            assert usage == ({"total_tokens": 2} if ending == "finish_reason" else {})
    finally:
        await response.aclose()
    assert events == [{"event": "token", "data": "part"}]


@pytest.mark.asyncio
@pytest.mark.parametrize("native", [False, True])
@pytest.mark.parametrize("ending", ["complete", "interrupt", "cancel"])
async def test_live_tokens_arrive_before_provider_finishes_and_connection_closes(tmp_path, native, ending):
    import asyncio
    received = asyncio.Event()
    release = asyncio.Event()
    closed = asyncio.Event()
    events = []

    class Body(httpx.AsyncByteStream):
        async def __aiter__(self):
            if native:
                first = json.dumps({"message": {"content": "First "}, "done": False}) + "\n"
                last = json.dumps({"message": {"content": "last"}, "done": True, "done_reason": "stop", "eval_count": 2}) + "\n"
            else:
                first = 'data: {"choices":[{"delta":{"content":"First "}}]}\n\n'
                last = 'data: {"choices":[{"delta":{"content":"last"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n'
            yield first.encode()
            await release.wait()
            if ending == "complete":
                yield last.encode()
        async def aclose(self):
            closed.set()

    def handler(request):
        assert json.loads(request.content)["stream"] is True
        return httpx.Response(200, stream=Body())

    async def sink(event):
        events.append(event)
        if event["event"] == "token":
            received.set()

    transport, client = _transport(tmp_path, handler)
    task = asyncio.create_task(transport.complete(
        _resolved(chat_protocol="native_chat_v1" if native else None),
        InferenceRequest(messages=({"role": "user", "content": "hello"},), max_output_tokens=32),
        token_sink=sink,
    ))
    try:
        await asyncio.wait_for(received.wait(), 2)
        assert not task.done()
        assert [event["data"] for event in events if event["event"] == "token"] == ["First "]
        if ending == "cancel":
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
        else:
            release.set()
            if ending == "interrupt":
                with pytest.raises(ModelTransportError, match="UPSTREAM_STREAM_INTERRUPTED"):
                    await task
            else:
                result = await task
                assert result.text == "First last"
        assert closed.is_set()
    finally:
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        await client.aclose()
