"""The same public browser/CLI contract with no application Python server."""
import hashlib
import io
import json
import sqlite3

import pytest
from PIL import Image

from tests.test_light_native_chat_turn import HEADERS, PATH, native_turn, source_pdf
from proxy.services.model_connection_contracts import ConnectionLocality, ConnectionRole

PUBLIC = '/api/chat'
STREAM = '/api/chat/stream'
pytestmark = pytest.mark.parametrize('native_turn', ['public'], indirect=True)


def events(response):
    assert response.raise_for_status().headers['x-les-handler'] == 'rust.chat'
    assert response.headers['content-type'].startswith('text/event-stream')
    return [(event.removeprefix('event: '), json.loads(data.removeprefix('data: ')))
            for event, data in (frame.splitlines() for frame in response.text.strip().split('\n\n'))]


def test_public_auth_and_key_replay_share_json_sse_but_not_callers(native_turn):
    t = native_turn
    body = {'question': 'Публичный чат 日本語', 'session_id': 'public-fixture'}
    t.state.joinpath('data').mkdir()
    with sqlite3.connect(t.state/'data/les_meta.db') as db:
        db.execute('CREATE TABLE auth_keys(key_value TEXT PRIMARY KEY,role TEXT,expires_at TEXT,is_active INTEGER)')
        db.executemany('INSERT INTO auth_keys VALUES (?,\'user\',NULL,1)', [('fixture-a',), ('fixture-b',)])
    forwarded = {'X-Forwarded-For': '192.0.2.10'}
    assert t.client.post(PUBLIC, headers=forwarded, json=body).status_code == 401
    assert t.client.post(PUBLIC, headers={'Origin': 'https://example.invalid'}, json=body).status_code == 403
    assert t.client.post(PUBLIC, headers={'Content-Type': 'text/plain'}, content=json.dumps(body)).status_code == 415
    assert t.client.get(PUBLIC).status_code == 405
    pending = t.client.get('/api/light/unsupported-fixture')
    assert pending.status_code == 501 and pending.json()['code'] == 'NATIVE_ROUTE_NOT_AVAILABLE'
    assert not t.seen
    key = 'public-repeat-key'
    a = {**forwarded, 'X-Api-Key': 'fixture-a', 'Idempotency-Key': key}
    b = {**forwarded, 'Authorization': 'Bearer fixture-b', 'Idempotency-Key': key}
    response = t.client.post(PUBLIC, json=body, headers=a).raise_for_status()
    assert response.headers['x-les-handler'] == 'rust.chat'
    first = response.json()
    assert events(t.client.post(STREAM, json=body, headers=a)) == [('final', first)]
    assert len(t.seen) == 1
    second = t.client.post(PUBLIC, json=body, headers=b).raise_for_status().json()
    assert second['history_id'] != first['history_id']
    assert len(t.seen) == 2
    assert t.client.post(PUBLIC, json={**body, 'session_id': 'other'}, headers=a).status_code == 409
    internal = t.client.post(PATH, json=body, headers={**HEADERS, 'Idempotency-Key': key}).raise_for_status().json()
    assert internal['history_id'] not in (first['history_id'], second['history_id'])
    with sqlite3.connect(t.database) as db:
        rows = db.execute('SELECT caller_scope,key_sha FROM les_chat_caller_keys').fetchall()
    assert len(rows) == 2 and rows[0][0] != rows[1][0]
    assert {row[1] for row in rows} == {hashlib.sha256(key.encode()).hexdigest()}
    assert not any('fixture-' in row[0] for row in rows)


def test_public_page_links_and_errors_survive_history_reload(native_turn):
    t = native_turn
    original, digest = source_pdf(t)
    sid = t.client.post('/api/workspace/sessions', json={'title': 'Проверка ссылки'}).raise_for_status().json()['session_id']
    body = {'question': 'Прочти страницу', 'session_id': sid, 'scope': {
        'scope_type': 'datasets', 'dataset_ids': ['ds'], 'selected_sources_only': True}}
    read = {'tool': 'read_pdf_source', 'args': {'doc_id': 'registered-document', 'page': 2}}
    t.scripts.extend([read, {'text': 'Прочитано [Источник 1]'},
                      read, {'text': 'Полученная часть', 'broken': True}])
    origin = {'Origin': str(t.client.base_url).rstrip('/')}
    good = events(t.client.post(STREAM, json=body, headers=origin))
    final = next(value for event, value in good if event == 'final')
    assert final['completion_status'] == 'completed'
    assert final['retrieval_trace']['citation_check']['linked'] == [1]
    source = final['source_map'][0]
    assert source['source_sha256'] == digest and source['url'].endswith('#page=2')
    assert source['locator']['source_sha256'] == digest
    assert t.client.get(source['url']).raise_for_status().content == original.read_bytes()
    datasets=t.client.get('/api/documents/datasets',params={'q':'fIXtUre','limit':1}).raise_for_status().json()
    assert [d['id'] for d in datasets['datasets']]==['ds']
    base='/api/documents/by-id/registered-document'
    identity={'source_sha256':digest,'dataset_id':'ds','doc_name':'folder/Source.pdf'}
    metadata=t.client.get(base,params=identity).raise_for_status()
    assert metadata.headers['x-les-handler']=='rust.documents'
    assert metadata.json()['document']['file_name']=='folder/Source.pdf'
    def preview(page,**options):
        return t.client.get(f'{base}/pdf-contour/pages/{page}/preview',params={**identity,**options})
    images=[preview(page,width=320).raise_for_status() for page in [1,2]]
    assert all(r.headers['content-type']=='image/png' and r.headers['x-les-handler']=='rust.documents' for r in images)
    with Image.open(io.BytesIO(images[0].content)) as first, Image.open(io.BytesIO(images[1].content)) as second:
        assert first.size==second.size and first.tobytes()!=second.tobytes()
        assert second.getextrema()!=((255,255),)*3
    with Image.open(io.BytesIO(preview(2,width=320,bbox='20,20,100,80').raise_for_status().content)) as crop:
        assert crop.width/crop.height==pytest.approx(4/3,abs=.02)
    assert preview(3).status_code==400
    assert preview(2,bbox='20,20,0,0').status_code==400
    assert preview(2,source_sha256='0'*64).status_code==409
    assert original.read_bytes()==t.client.get(source['url']).raise_for_status().content
    broken = events(t.client.post(STREAM, json=body, headers=origin))
    assert broken[-1][0] == 'error' and broken[-1][1]['code'] == 'UPSTREAM_STREAM_INTERRUPTED'
    assert all(event != 'final' for event, _ in broken)
    t.children[-1].kill()
    t.children[-1].wait(timeout=10)
    t.start()
    history = t.client.get('/api/chat/history', params={'session_id': sid}).raise_for_status().json()
    assert history[1]['meta']['source_map'][0]['url'] == source['url']
    assert history[-1]['meta']['source_map'][0]['url'] == source['url']
    assert history[-1]['meta']['error']['code'] == 'UPSTREAM_STREAM_INTERRUPTED'
    assert history[-1]['text'] == 'Полученная часть'


def test_selected_sources_only_never_streams_a_claim_without_evidence(native_turn):
    t = native_turn
    source_pdf(t)
    t.scripts.append({'text': 'Неподтверждённое утверждение'})
    body = {'question': 'Что в документе?', 'mode': 'search', 'scope': {'scope_type': 'dataset',
        'dataset_ids': ['ds'], 'selected_sources_only': True}}
    stream = events(t.client.post(STREAM, json=body))
    assert all(event not in ('token', 'final') for event, _ in stream)
    assert stream[-1][1]['code'] == 'SOURCE_EVIDENCE_MISSING'
    with sqlite3.connect(t.database) as db:
        assert db.execute('SELECT answer,success FROM chat_history').fetchall() == [('', 0)]


def test_pending_options_fail_before_even_a_deterministic_command(native_turn):
    t = native_turn
    for field in ['reranker_enabled', 'validation_enabled', 'semantic_cache_enabled']:
        response = t.client.post(STREAM, json={'question': '/help', field: True})
        assert response.status_code == 503 and response.json()['code'].endswith('_CAPABILITY_PENDING')
    assert not t.seen
    with sqlite3.connect(t.database) as db:
        assert db.execute("SELECT COUNT(*) FROM sqlite_master WHERE name='les_chat_runs'").fetchone()[0] == 0


def test_public_candidate_blocks_remote_binding_before_dns_or_credentials(native_turn):
    t = native_turn
    remote = t.registry.create_connection(
        display_name='Remote fixture', base_url='https://must-not-resolve.invalid/v1',
        model_id='remote-fixture', locality=ConnectionLocality.REMOTE, requested_context_tokens=8192,
        secret_ref='env:ABSENT_FIXTURE_SECRET', extension_type=None, actor='admin:test')
    binding = t.registry.get_role_binding(ConnectionRole.ANSWER)
    t.registry.bind_role(ConnectionRole.ANSWER, remote.revision_id,
                         expected_binding_revision=binding.binding_revision, actor='admin:test')
    response = t.client.post(PUBLIC, json={'question': 'Приватный тестовый текст'})
    assert response.status_code == 503 and response.json()['code'] == 'CLOUD_CONSENT_REQUIRED'
    assert not t.seen
    # Allowing the provider does not authorize sending local-only material.
    with (t.state/'.env').open('a',encoding='utf-8') as file:file.write('LES_CLOUD_CONSENT=true\n')
    source_pdf(t)
    with sqlite3.connect(t.database) as db:
        db.execute("ALTER TABLE datasets ADD sensitivity TEXT DEFAULT 'P0'")
    body={'question':'Проверка чувствительности','dataset_ids':['ds']}
    assert t.client.post(PUBLIC,json=body).json()['code']=='CLOUD_SOURCE_LOCAL_ONLY'
    assert t.client.post(PUBLIC,json={**body,'attachment_context':'PRIVATE'}).json()['code']=='CLOUD_ATTACHMENT_LOCAL_ONLY'
    with sqlite3.connect(t.database) as db:db.execute("UPDATE datasets SET sensitivity='P1'")
    # The invalid synthetic reference proves we reached credential
    # resolution without reading a real key or opening a connection.
    assert t.client.post(PUBLIC,json=body).json()['code']=='SECRET_REF_NOT_ALLOWED'
    assert not t.seen
