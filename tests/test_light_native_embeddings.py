"""Native ONNX worker acceptance and fail-closed compatibility selection."""
import hashlib
import json
import os
from pathlib import Path
from types import SimpleNamespace

import pytest
from backend import native_core, native_embedding
from backend.embedding_client import EmbedClient


@pytest.mark.asyncio
async def test_active_binding_uses_native_worker_and_never_retries_failed_inference_via_http(monkeypatch):
    from tests.test_model_connection_embeddings_integration import Resolver, Transport, _response
    connection = SimpleNamespace(revision_id='local:revision-3', model_id='bge-m3')
    transport = Transport(_response([[1., 0.]], model='bge-m3'))
    client = EmbedClient('http://fixture', model='bge-m3', connection_mode='active',
                         connection_resolver=Resolver(connection), connection_transport=transport)
    client._index_vector_size = 2
    monkeypatch.setattr(native_embedding, 'configured', lambda: True)
    observed = []
    def encode(model, texts, dimension, **options):
        observed.append((model, texts, dimension, options))
        return [[1., 0.]]
    monkeypatch.setattr(native_embedding, 'encode', encode)
    assert await client.encode_async(['Текст']) == [[1., 0.]]
    assert observed == [('bge-m3', ['Текст'], 2, {'release_after': False, 'connection_revision_id': 'local:revision-3'})]
    assert not transport.calls
    def fail(*args, **kwargs):
        raise RuntimeError('EMBEDDING_CONNECTION_REVISION_MISMATCH')
    monkeypatch.setattr(native_embedding, 'encode', fail)
    with pytest.raises(RuntimeError, match='EMBEDDING_CONNECTION_REVISION_MISMATCH'):
        await client.encode_async(['Другой текст'])
    assert not transport.calls


def test_native_model_worker_reuse_revision_guard_and_restart(tmp_path):
    executable = os.getenv('LES_TEST_NATIVE_API')
    specification = os.getenv('LES_TEST_NATIVE_EMBEDDING_SPEC')
    if not executable or not specification:
        pytest.skip('Set native API, core and an offline embedding specification')
    spec = json.loads(Path(specification).read_text(encoding='utf-8'))
    spec['connection_revision_id'] = 'fixture:bound-revision'
    path = tmp_path/'embedding.json'
    path.write_text(json.dumps(spec), encoding='utf-8')
    def call(**fields):
        return native_core.required('worker_embeddings', request={
            'schema':'les.worker-host.v1','program':str(Path(executable).resolve()),
            'directory':str(tmp_path),'args':['--serve-embeddings', str(path)],
            'timeout_ms':120000,'context':{'sha256':hashlib.sha256(path.read_bytes()).hexdigest()},
            'input':{'model_id':spec['model_id'],'dimension':spec['dimension'],
                'connection_revision_id':'fixture:bound-revision','texts':['Проверка кабеля'], **fields}})
    try:
        first = call()
        assert first['status']=='completed' and first['result']['status']=='ok', first
        vector = first['result']['result']['vectors'][0]
        assert len(vector)==spec['dimension']
        assert sum(v*v for v in vector) == pytest.approx(1., abs=1e-5)
        wrong = call(connection_revision_id='another:revision')
        assert wrong['worker_pid']==first['worker_pid'] and wrong['reused'] is True
        assert wrong['result']=={'status':'failed','code':'EMBEDDING_CONNECTION_REVISION_MISMATCH'}
        mismatch = call(model_id='another-model')
        assert mismatch['result']=={'status':'failed','code':'EMBEDDING_INDEX_MODEL_MISMATCH'}
        import psutil
        owned = psutil.Process(first['worker_pid'])
        assert Path(owned.exe()).resolve()==Path(executable).resolve()
        owned.kill(); owned.wait(timeout=10)
        restarted = call()
        assert restarted['status']=='completed' and restarted['result']['status']=='ok', restarted
        assert restarted['worker_pid']!=first['worker_pid'] and restarted['reused'] is False
        assert restarted['result']['result']['vectors'][0]==pytest.approx(vector, abs=1e-6)
    finally:
        native_core.required('worker_release_idle')
