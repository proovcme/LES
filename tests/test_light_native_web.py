"""Native web search keeps provenance and survives loss of the retained application server."""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from urllib.parse import parse_qs, urlsplit

from test_light_native_api import gateway, MODEL_BRIDGE_TOKEN


def test_native_search_keeps_exact_sources(gateway):
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
        response = client.post('/api/light/internal-web', headers={'X-Les-Model-Token': MODEL_BRIDGE_TOKEN}, json={
            'schema': 'les.web.v1', 'operation': 'search', 'mode': 'extended',
            'searxng_url': f'http://127.0.0.1:{server.server_port}',
            'q': 'ГОСТ 123 日本語 4.2', 'limit': 4,
        })
        result = response.raise_for_status().json()
        assert result['status'] == 'ok' and result['provider'] == 'searxng'
        assert len(result['results']) == 2 and result['retrieved_at']
        source = result['results'][0]
        assert source['title'] == 'Норматив — 日本語'
        assert source['url'] == 'https://example.org/norm/a%2Fb'
        assert source['snippet'] == 'Пункт 4.2: подтверждённый текст.'
        assert parse_qs(urlsplit(seen[0]).query)['q'] == ['ГОСТ 123 日本語 4.2']
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
