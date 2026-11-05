"""Pressure recovery, concurrent generation ownership and durable native jobs."""
import asyncio
from types import SimpleNamespace
import pytest
import httpx
from backend import native_core
from proxy.services import request_memory_service as memory
from tests.test_light_format_worker import native, worker

@pytest.mark.parametrize('changes',[
 {'locality':'remote'}, {'extension_type':'openai-compatible'}, {'secret_ref':'configured'},
 {'base_url':'http://192.0.2.1:11434/v1'}, {'base_url':'http://user:pass@127.0.0.1:11434/v1'},
])
def test_no_unload_of_remote_custom_or_authenticated_connection(changes):
    data=dict(locality='loopback',extension_type='ollama',secret_ref=None,base_url='http://127.0.0.1:11434/v1')
    data.update(changes)
    assert memory.local_ollama_endpoint(SimpleNamespace(**data)) is None

def _connection():
    return SimpleNamespace(locality='loopback',extension_type='ollama',secret_ref=None,base_url='http://127.0.0.1:11434/v1',model_id='explicit-model',revision_id='fixed:r1')

@pytest.mark.parametrize('phase', ['chat_end', 'chat_prepare'])
@pytest.mark.parametrize('free,busy,expected',[(3.,False,1),(5.,False,0),(3.,True,0)])
def test_only_request_model_is_unloaded_under_pressure_and_without_competing_generation(native,monkeypatch,free,busy,expected,phase):
    from backend import runtime_memory
    import psutil,json
    monkeypatch.setattr(runtime_memory,'collect_transient',lambda phase:{'free_after_gb':free})
    from backend import system_memory
    monkeypatch.setattr(system_memory,'system_memory_snapshot',lambda:{'ram_free_gb':6.})
    calls=[];original=httpx.AsyncClient
    def handle(req):
        calls.append(req)
        assert str(req.url)=='http://127.0.0.1:11434/api/generate'
        assert json.loads(req.content)=={'model':'explicit-model','keep_alive':0}
        return httpx.Response(200,json={'done':True})
    monkeypatch.setattr(memory.httpx,'AsyncClient',lambda **kwargs:original(transport=httpx.MockTransport(handle),**kwargs))
    async def run():
        state=SimpleNamespace(llm_semaphore=asyncio.Semaphore(0 if busy else 1))
        result=await memory.release_after_request(_connection(),state,phase=phase)
        assert result['released'] is bool(expected)
        assert len(calls)==expected
    asyncio.run(run())

def test_request_cleanup_failure_preserves_answer_and_resets_ownership(monkeypatch):
    from proxy.services import chat_runtime
    monkeypatch.setattr(chat_runtime,'get_chat_state',lambda:object())
    async def fail(*args):raise OSError('cleanup failed')
    monkeypatch.setattr(memory,'release_after_request',fail)
    async def run():
        async with memory.managed_request_memory():
            assert memory.remember_invocation(_connection())
            # A spawned task owns a separate lifetime even though contextvars copy.
            async def child():return memory.remember_invocation(_connection())
            assert await asyncio.create_task(child()) is False
            answer='completed answer'
        assert memory._owned.get()is None
        return answer
    assert asyncio.run(run())=='completed answer'



def test_native_release_does_not_cancel_active_worker_and_releases_idle_one(worker,tmp_path):
    import psutil,time
    from concurrent.futures import ThreadPoolExecutor
    first=worker()['result']['pid']
    r=native_core.required('memory',request={'action':'reclaim','release_worker':True})
    assert r['worker']['released']
    deadline=time.monotonic()+2
    while psutil.pid_exists(first)and time.monotonic()<deadline:time.sleep(.01)
    assert not psutil.pid_exists(first)
    marker=tmp_path/'running'
    with ThreadPoolExecutor(max_workers=1)as pool:
        future=pool.submit(worker,'descendant',timeout=800,marker=str(marker))
        deadline=time.monotonic()+2
        while not marker.exists()and time.monotonic()<deadline:time.sleep(.01)
        assert marker.exists()
        r=native_core.required('memory',request={'action':'reclaim','release_worker':True})
        assert r['worker']['busy'] and not r['worker']['released']
        assert future.result()['code']=='WORKER_TIMEOUT'
    assert worker()['status']=='completed'

def test_queue_failure_reclaims_without_masking_original(monkeypatch):
    from proxy.services import dataset_parse_service as service
    from backend import runtime_memory
    calls=[]
    monkeypatch.setattr(runtime_memory,'collect_transient',lambda phase:calls.append(phase))
    async def fail(*args):raise RuntimeError('original parse failure')
    monkeypatch.setattr(service,'_run_parse_scheduler_inner',fail)
    with pytest.raises(RuntimeError,match='original parse failure'):asyncio.run(service.run_parse_scheduler(None,None))
    assert calls==['queue_end']

@pytest.mark.parametrize('fail',[False,True])
def test_parse_boundary_reclaims_on_upload_path_success_and_error(monkeypatch, fail):
    from backend.qdrant_ingestion import QdrantIngestion
    from backend import runtime_memory
    calls=[];statuses=[]
    monkeypatch.setattr(runtime_memory,'collect_transient',lambda phase:calls.append(phase)or {'released':True})
    async def ensure():pass
    def parse(*args):
        if fail:raise RuntimeError('conversion failed')
        return {'status':'completed','errors':0,'remaining_pending':0}
    adapter=SimpleNamespace(_ensure_collection=ensure,_assert_dense_index_contract=lambda:None,
        _sync_parse=parse,db=SimpleNamespace(update_dataset_status=lambda *args:statuses.append(args)))
    if fail:
        with pytest.raises(RuntimeError,match='conversion failed'):asyncio.run(QdrantIngestion.parse_dataset(adapter,'ds',limit=1))
        assert statuses[-1]==('ds','ERROR')
    else:
        result=asyncio.run(QdrantIngestion.parse_dataset(adapter,'ds',limit=1))
        assert result['memory_cleanup']=={'released':True} and statuses[-1]==('ds','COMPLETED')
    assert calls==['batch_end']

def test_index_residency_tracks_actual_frozen_model_across_thread_boundary(monkeypatch):
    import json
    from backend import embedding_residency,system_memory
    from tests.test_light_embedding_762 import cached_client
    client,resolver,transport=cached_client()
    connection=resolver.connection
    connection.locality='loopback';connection.extension_type='ollama';connection.secret_ref=None
    connection.base_url='http://127.0.0.1:11434/v1'
    connection.endpoint=SimpleNamespace(canonical_base_url=connection.base_url)
    connection.capability_snapshot=SimpleNamespace(transport_options={'embedding_protocol':'native_embedding_v1'},snapshot_id='cap:1')
    calls=[];original=httpx.AsyncClient
    def handler(req):
        calls.append((req.url.path,json.loads(req.content)))
        return httpx.Response(200,json={'model':connection.model_id,'embeddings':[]})
    monkeypatch.setattr(embedding_residency.httpx,'AsyncClient',lambda **kw:original(transport=httpx.MockTransport(handler),**kw))
    monkeypatch.setattr(system_memory,'system_memory_snapshot',lambda:{'ram_free_gb':8.})
    async def run():
        token,owned=embedding_residency.begin()
        try:
            await asyncio.to_thread(client.encode_sync,['synthetic'])
            assert list(owned.values())==[connection]
            result=await embedding_residency.release(owned)
            assert result=={'released':1,'failed':0}and not owned
        finally:embedding_residency.end(token)
    asyncio.run(run())
    assert calls==[('/api/embed',{'model':'embed-model','input':[],'keep_alive':0})]


def test_cache_only_scope_never_unloads_an_assigned_but_unused_model(monkeypatch):
    from backend import embedding_residency
    def forbidden(**kwargs):raise AssertionError('unused model must not be contacted')
    monkeypatch.setattr(embedding_residency.httpx,'AsyncClient',forbidden)
    async def run():
        token,owned=embedding_residency.begin()
        try:assert await embedding_residency.release(owned)=={'released':0,'failed':0}
        finally:embedding_residency.end(token)
    asyncio.run(run())

def test_cancelled_parse_does_not_unload_model_of_continuing_thread(monkeypatch):
    import threading,time
    from backend.qdrant_ingestion import QdrantIngestion
    from backend import embedding_residency,runtime_memory
    started=threading.Event();release_calls=[]
    monkeypatch.setattr(runtime_memory,'collect_transient',lambda phase:{})
    async def release(owned):release_calls.append(True);return {}
    monkeypatch.setattr(embedding_residency,'release',release)
    async def ensure():pass
    def parse(*args):started.set();time.sleep(.15);return {'status':'completed'}
    adapter=SimpleNamespace(_ensure_collection=ensure,_assert_dense_index_contract=lambda:None,
        _sync_parse=parse,db=SimpleNamespace(update_dataset_status=lambda *args:None))
    async def run():
        task=asyncio.create_task(QdrantIngestion.parse_dataset(adapter,'ds',limit=1))
        while not started.is_set():await asyncio.sleep(.001)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):await task
    asyncio.run(run())
    assert release_calls==[]

@pytest.mark.parametrize('base',['http://127.0.0.1:11434/v1','http://127.0.0.1:11434/api/v1','http://127.0.0.1:11434'])
def test_ollama_residency_uses_canonical_native_path(base):
    connection=_connection();connection.base_url=base
    assert memory.local_ollama_endpoint(connection)=='http://127.0.0.1:11434/api/generate'


@pytest.mark.parametrize('recovers', [False, True])
def test_unmapping_wait_ignores_brief_threshold_crossing_and_has_deadline(monkeypatch, recovers):
    from backend import runtime_memory, system_memory
    from proxy.services import runtime_admission
    clock = [0.0]
    readings = [4.1, 3.8, 4.2, 3.9, 4.3, 4.5, 4.6, 4.7] if recovers else [3.8] * 21
    samples = []
    def sample():
        value = readings[min(len(samples), len(readings) - 1)]
        samples.append(value)
        return {'ram_free_gb': value}
    async def sleep(seconds):
        clock[0] += seconds
    monkeypatch.setattr(runtime_memory.time, 'monotonic', lambda: clock[0])
    monkeypatch.setattr(runtime_memory.asyncio, 'sleep', sleep)
    monkeypatch.setattr(system_memory, 'system_memory_snapshot', sample)
    monkeypatch.setattr(runtime_admission, 'chat_min_free_gb', lambda: 4.0)
    result = asyncio.run(runtime_memory.wait_for_chat_memory())
    assert result['ready'] is recovers
    assert result['seconds'] == (1.75 if recovers else 5.0)
    assert len(samples) == (8 if recovers else 21)


@pytest.mark.parametrize('changes,resolve_fails,expected', [
    ({}, False, True), ({'locality': 'remote'}, False, False),
    ({'secret_ref': 'configured'}, False, False),
    ({'extension_type': 'openai-compatible'}, False, False),
    ({}, True, False),
])
def test_preflight_only_prepares_validated_explicit_answer_role(monkeypatch, changes, resolve_fails, expected):
    from proxy.services import chat_inference_service
    from proxy.services.model_connection_contracts import ConnectionRole, CapabilityName
    connection = _connection()
    for key, value in changes.items():
        setattr(connection, key, value)
    resolved = []
    released = []
    def binding(role):
        assert role is ConnectionRole.ANSWER
        return SimpleNamespace(connection_revision_id='fixed:r1')
    def resolve(revision, *, required_capabilities):
        resolved.append(revision)
        assert required_capabilities == frozenset({CapabilityName.CHAT_COMPLETIONS})
        if resolve_fails:
            raise ValueError('capability expired')
        return connection
    resolver = SimpleNamespace(registry=SimpleNamespace(
        get_role_binding=binding, get_revision=lambda rid: connection), resolve_revision=resolve)
    monkeypatch.setattr(chat_inference_service, '_model_connection_resolver', lambda: (resolver, None))
    async def release(actual, state, sink, *, phase):
        assert actual is connection and phase == 'chat_prepare'
        released.append(actual)
        return {'released': True}
    monkeypatch.setattr(memory, 'release_after_request', release)
    result = asyncio.run(memory.prepare_assigned_model(object()))
    assert result['released'] is expected
    assert bool(released) is expected
    assert bool(resolved) is (not changes)
