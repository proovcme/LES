"""Embedding protocol admission, bounded query cache and residency policy."""
from dataclasses import replace
from types import SimpleNamespace
import json

import httpx
import pytest

import backend.embedding_client as module
from backend.embedding_client import EmbedClient
from proxy.services.model_capability_service import CapabilityProbe
from proxy.services.model_connection_contracts import CapabilityName
from proxy.services.openai_compatible_transport_service import EmbeddingResponse, ModelTransportError
from tests.test_model_capability_service import _connection, _peer_ok
from tests.test_openai_compatible_transport_service import _resolved, _transport


@pytest.mark.asyncio
@pytest.mark.parametrize('native_status,native_payload,accepted', [
    (200, {'model': 'qwen-test', 'embeddings': [[.1, .2]]}, True),
    (404, {'error': 'missing'}, False),
    (200, [], False),
    (200, {'model': 'another-model', 'embeddings': [[.1]]}, False),
    (200, {'model': 'qwen-test', 'embeddings': [[True]]}, False),
    (200, {'model': 'qwen-test', 'embeddings': [[]]}, False),
    (200, {'model': 'qwen-test', 'embeddings': [[float('nan')]]}, False),
])
async def test_native_protocol_requires_live_valid_response(native_status, native_payload, accepted):
    calls = []
    def handler(request):
        body = json.loads(request.content)
        calls.append((request.url.path, body))
        if request.url.path == '/v1/embeddings':
            return httpx.Response(200, json={'model': 'qwen-test', 'data': [{'index': 0, 'embedding': [.1]}]})
        # NaN is deliberately malformed upstream JSON, not an httpx encoding.
        return httpx.Response(native_status, content=json.dumps(native_payload).encode())
    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        snapshot = await CapabilityProbe(client=client, peer_verifier=_peer_ok).probe(
            _connection(extension_type='ollama'), requested=[CapabilityName.EMBEDDINGS])
    assert (snapshot.transport_options.get('embedding_protocol') == 'native_embedding_v1') is accepted
    native = next(body for path, body in calls if path == '/api/embed')
    assert native['truncate'] is False
    assert native['keep_alive'] == 0
    assert native['options']['num_ctx'] == 8192


@pytest.mark.asyncio
@pytest.mark.parametrize('release_after,keep_alive', [(False, '60s'), (True, 0)])
async def test_native_embedding_context_and_release_policy(tmp_path, release_after, keep_alive):
    def handler(request):
        assert request.url.path == '/api/embed'
        assert json.loads(request.content) == {
            'model': 'model-1', 'input': ['alpha'], 'truncate': False,
            'keep_alive': keep_alive, 'options': {'num_ctx': 8192}}
        return httpx.Response(200, json={'model': 'model-1', 'embeddings': [[.1, .2]]})
    resolved = _resolved()
    connection = replace(resolved, capability_snapshot=replace(
        resolved.capability_snapshot, transport_options={'embedding_protocol': 'native_embedding_v1'}))
    transport, client = _transport(tmp_path, handler)
    async with client:
        response = await transport.embed(connection, ['alpha'], release_after=release_after)
    assert response.vectors == ((.1, .2),)


@pytest.mark.asyncio
async def test_native_context_rejection_never_retries_truncated_compatible(tmp_path):
    calls = []
    def handler(request):
        calls.append(request.url.path)
        return httpx.Response(400, json={'error': 'input exceeds context'})
    resolved = _resolved()
    connection = replace(resolved, capability_snapshot=replace(
        resolved.capability_snapshot, transport_options={'embedding_protocol': 'native_embedding_v1'}))
    transport, client = _transport(tmp_path, handler)
    async with client:
        with pytest.raises(ModelTransportError, match='UPSTREAM_HTTP_ERROR: 400'):
            await transport.embed(connection, ['long input'], release_after=True)
    assert calls == ['/api/embed']


class Resolver:
    def __init__(self):
        self.connection = SimpleNamespace(revision_id='conn:r1', model_id='embed-model')
        self.calls = 0
        self.error = None

    def resolve(self, *args, **kwargs):
        self.calls += 1
        if self.error:
            raise self.error
        return self.connection


class Transport:
    def __init__(self):
        self.calls = []
        self.error = None

    async def embed(self, connection, inputs, **kwargs):
        self.calls.append((connection.revision_id, tuple(inputs), kwargs))
        if self.error:
            raise self.error
        return EmbeddingResponse(vectors=tuple((.1, .2) for _ in inputs), model_id='embed-model', usage={})


def cached_client():
    resolver, transport = Resolver(), Transport()
    client = EmbedClient('http://unused', model='embed-model', connection_mode='active',
                         connection_resolver=resolver, connection_transport=transport)
    return client, resolver, transport


@pytest.mark.asyncio
async def test_cache_returns_copies_checks_binding_and_expires(monkeypatch):
    client, resolver, transport = cached_client()
    clock = [100.0]
    monkeypatch.setattr(module.time, 'monotonic', lambda: clock[0])
    first = await client.encode_async(['synthetic'], query=True)
    first[0][0] = 9
    assert await client.encode_async(['synthetic'], query=True) == [[.1, .2]]
    assert len(transport.calls) == 1 and resolver.calls == 2
    resolver.error = RuntimeError('binding disabled')
    with pytest.raises(RuntimeError, match='binding disabled'):
        await client.encode_async(['synthetic'], query=True)
    resolver.error = None
    clock[0] += module.QUERY_CACHE_TTL
    await client.encode_async(['synthetic'], query=True)
    assert len(transport.calls) == 2
    resolver.connection.revision_id = 'conn:r2'
    await client.encode_async(['synthetic'], query=True)
    assert len(transport.calls) == 3


@pytest.mark.asyncio
async def test_cache_is_bounded_query_only_and_does_not_store_failure():
    client, resolver, transport = cached_client()
    for index in range(module.QUERY_CACHE_SIZE + 2):
        await client.encode_async([f'synthetic-{index}'], query=True)
    assert len(client._query_cache) == module.QUERY_CACHE_SIZE
    assert 'synthetic-' not in repr(list(client._query_cache))
    await client.encode_async(['synthetic-0'], query=True)
    assert len(transport.calls) == module.QUERY_CACHE_SIZE + 3
    await client.encode_async(['document'])
    await client.encode_async(['document'])
    assert len(transport.calls) == module.QUERY_CACHE_SIZE + 5
    transport.error = ModelTransportError('upstream unavailable')
    with pytest.raises(ModelTransportError):
        await client.encode_async(['uncached failure'], query=True)
    transport.error = None
    await client.encode_async(['uncached failure'], query=True)
    assert len(transport.calls) == module.QUERY_CACHE_SIZE + 7


@pytest.mark.asyncio
async def test_last_query_batch_only_releases_model():
    client, resolver, transport = cached_client()
    resolver.connection.capability_snapshot = SimpleNamespace(
        transport_options={'embedding_protocol': 'native_embedding_v1'}, snapshot_id='cap:1')
    await client.encode_async([f'synthetic-{n}' for n in range(17)], query=True)
    assert [call[2] for call in transport.calls] == [{'release_after': False}, {'release_after': True}]


@pytest.mark.asyncio
@pytest.mark.parametrize('change', ['probe', 'context', 'dimension', 'endpoint', 'normalization'])
async def test_cache_contract_changes_cannot_reuse_old_vector(change, monkeypatch):
    client, resolver, transport = cached_client()
    resolver.connection.capability_snapshot = SimpleNamespace(snapshot_id='cap:1')
    resolver.connection.effective_preset = SimpleNamespace(input_token_limit=8192)
    resolver.connection.base_url = 'http://first'
    monkeypatch.setattr(module, 'prepare_query_for_embedding', lambda text: text)
    await client.encode_async(['synthetic'], query=True)
    if change == 'probe':
        resolver.connection.capability_snapshot.snapshot_id = 'cap:2'
    elif change == 'context':
        resolver.connection.effective_preset.input_token_limit = 4096
    elif change == 'dimension':
        client._index_vector_size = 2
    elif change == 'endpoint':
        resolver.connection.base_url = 'http://second'
    else:
        monkeypatch.setattr(module, 'prepare_query_for_embedding', lambda text: 'Instruction: ' + text)
    await client.encode_async(['synthetic'], query=True)
    assert len(transport.calls) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize('payload', [{'embeddings': [[.1]]}, []])
async def test_native_missing_model_is_rejected(tmp_path, payload):
    resolved = _resolved()
    connection = replace(resolved, capability_snapshot=replace(
        resolved.capability_snapshot, transport_options={'embedding_protocol': 'native_embedding_v1'}))
    transport, client = _transport(tmp_path, lambda request: httpx.Response(200, json=payload))
    async with client:
        with pytest.raises(ModelTransportError, match='UPSTREAM_EMBEDDING_RESPONSE_INVALID'):
            await transport.embed(connection, ['synthetic'])
