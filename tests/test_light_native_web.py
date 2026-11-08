"""Native web search keeps provenance and survives loss of the retained application server."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from urllib.parse import parse_qs, urlsplit

from test_light_native_api import gateway, wait_for, MODEL_BRIDGE_TOKEN
from proxy.services import local_model_http
from proxy.services.web_research_config_service import WebResearchConfig
from proxy.services.tool_harness_service import ToolHarness
from proxy.services.chat_evidence_tools import web_source_map_from_tool_results
from proxy.services.chat_capability_scope_service import filter_profile_tools


def test_native_search_through_chat_tool_keeps_exact_sources_without_backend(gateway, monkeypatch):
    client, _, _ = gateway
    seen = []
    class Search(BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def do_GET(self):
            seen.append(self.path)
            body = json.dumps({'results': [
                {'title': 'Норматив — 日本語', 'content': 'Пункт 4.2: подтверждённый текст.',
                 'url': 'https://example.org/norm/a%2Fb'},
                {'title': 'Дубликат', 'url': 'https://example.org/norm/a%2Fb'},
                {'title': 'Не источник', 'url': 'javascript:alert(1)'},
                {'title': 'Локальная служба', 'url': 'http://127.0.0.1/private'},
                {'title': 'Другой документ', 'content': 'Другая редакция.', 'url': 'https://example.org/second'},
            ]}, ensure_ascii=False).encode('utf-8')
            self.send_response(200); self.send_header('Content-Type', 'application/json; charset=utf-8')
            self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Search)
    thread = Thread(target=server.serve_forever, daemon=True); thread.start()
    try:
        monkeypatch.setattr(local_model_http, '_bridge', (str(client.base_url.join('/api/light/internal-model-http')), MODEL_BRIDGE_TOKEN))
        config = WebResearchConfig('extended', f'http://127.0.0.1:{server.server_port}', '', '')
        assert client.post('/test-stop-upstream').status_code == 200
        wait_for(lambda: client.get('/api/light/instance').status_code == 503)
        harness = ToolHarness(); harness._web_config = config
        result = harness.call('web_search', {'q': 'ГОСТ 123 日本語 4.2', 'limit': 4})
        assert result['status'] == 'ok', result
        assert len(result['result']['results']) == 2
        assert result['sources'][0]['title'] == 'Норматив — 日本語'
        assert result['sources'][0]['url'] == 'https://example.org/norm/a%2Fb'
        assert result['result']['provider'] == 'searxng'
        assert result['result']['retrieved_at']
        assert parse_qs(urlsplit(seen[0]).query)['q'] == ['ГОСТ 123 日本語 4.2']
        sources = web_source_map_from_tool_results([result])
        assert sources[0]['locator']['excerpt'] == 'Пункт 4.2: подтверждённый текст.'
        assert sources[0]['locator']['kind'] == 'web_result'
        assert sources[0]['locator']['url'] == 'https://example.org/norm/a%2Fb'
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=3)


def test_private_web_boundary_and_local_addresses_are_rejected(gateway):
    client, _, _ = gateway
    path = '/api/light/internal-web'; headers = {'X-Les-Model-Token': MODEL_BRIDGE_TOKEN}
    query = {'schema': 'les.web.v1', 'operation': 'search', 'q': 'x'}
    assert client.post(path, json=query).status_code == 404
    assert client.post(path, json=query, headers={**headers, 'Origin': str(client.base_url).rstrip('/')}).status_code == 404
    assert client.post(path, json=query, headers=headers).json()['status'] == 'missing'
    for url in ('http://127.0.0.1/private', 'http://169.254.169.254/latest/meta-data',
                'http://100.64.1.1/', 'http://[::1]/', 'file:///secret', 'https://user:secret@example.org/'):
        response = client.post(path, headers=headers, json={'schema': 'les.web.v1', 'operation': 'read', 'url': url})
        payload = response.raise_for_status().json()
        assert payload['status'] == 'error' and payload['code'] == 'WEB_URL_NOT_PUBLIC'
        assert payload['sources'] == [] and payload['text'] == ''
        assert payload['warnings']


def test_selected_sources_scope_blocks_both_web_tools():
    assert filter_profile_tools(['web_search', 'web_read', 'read_source'], selected_sources_only=True) == ['read_source']
    assert filter_profile_tools(['web_search', 'web_read'], selected_sources_only=False) == ['web_search', 'web_read']


def test_bridge_failure_is_an_error_not_an_empty_success(monkeypatch):
    monkeypatch.setattr(local_model_http, 'web_ready', lambda: True)
    def fail(*_args, **_kwargs): raise RuntimeError('WEB_RESPONSE_INVALID')
    monkeypatch.setattr(local_model_http, 'web_request', fail)
    result = ToolHarness().call('web_search', {'q': 'Официальная документация Rust'})
    assert result['status'] == 'error'
    assert result['warnings'] and result['sources'] == []
    assert result['result']['code'] == 'WEB_RUNTIME_UNAVAILABLE'
