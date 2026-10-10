import asyncio
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from proxy.services import runtime_admission as admission
from proxy.services.generation_guard_service import generation_guard
from proxy.services.model_resource_service import ModelResourceTarget, assigned_resource_target
from proxy.services.model_connection_contracts import ConnectionLocality, ConnectionRole
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.canonical_route_service import BoundModelChatRunner, CanonicalRouteMode
from proxy.services.openai_compatible_transport_service import InferenceRequest, ModelTransportError


def target(locality='loopback', model='selected'):
    return ModelResourceTarget('ollama', model, f'{model}:r1', locality)


def state(memory=12):
    return SimpleNamespace(current_mode={'mode':'chat'}, metrics_cache={'ram_free_gb':memory, 'swap_pct':0},
                           job_service=None, job_tracker={}, llm_semaphore=asyncio.Semaphore(1))


@pytest.fixture(autouse=True)
def policy(monkeypatch):
    for key in ('LES_CHAT_MEMORY_GUARD','LES_CHAT_MIN_FREE_GB','LES_CHAT_RESIDENT_MIN_FREE_GB'):
        monkeypatch.delenv(key, raising=False)
    monkeypatch.setattr('proxy.services.chat_runtime._active_dispatcher_reindex_jobs', lambda _:0)
    monkeypatch.setattr('proxy.services.generation_guard_service.live_memory_metrics', lambda metrics:dict(metrics))


@pytest.mark.parametrize('locality,allowed', [('loopback',False),('private_network',False),('unknown',False),('remote',True)])
def test_actual_locality_controls_memory_not_provider_or_environment(monkeypatch, locality, allowed):
    monkeypatch.setenv('LES_LLM_PROVIDER','openai')
    result=admission.evaluate_chat_admission(current_mode={'mode':'chat'},
        metrics_cache={'ram_free_gb':3.5,'swap_pct':0},connection=target(locality))
    assert result.allowed is allowed
    assert result.indexing_chat_policy['connection_revision']=='selected:r1'
    assert result.indexing_chat_policy['hard_min_free_gb']==4


def test_loaded_name_cannot_disable_hard_floor(monkeypatch):
    monkeypatch.setenv('LES_CHAT_RESIDENT_MIN_FREE_GB','0')
    result=admission.evaluate_chat_admission(current_mode={'mode':'chat'},
        metrics_cache={'ram_free_gb':0.6,'swap_pct':0,'llm_loaded_models':['selected']},connection=target())
    assert not result.allowed and '0.6 < 4.0' in result.reason


def test_registry_bound_snapshot_ignores_legacy_environment(tmp_path, monkeypatch):
    monkeypatch.setenv('RAG_META_DB_PATH',str(tmp_path/'roles.db'))
    monkeypatch.setenv('LES_LLM_PROVIDER','mlx')
    assert assigned_resource_target().provider=='unassigned'
    registry=ModelConnectionRegistry()
    first=registry.create_connection(display_name='API',base_url='https://api.example.com/v1',model_id='bound',
        locality=ConnectionLocality.REMOTE,requested_context_tokens=None,secret_ref=None,extension_type=None,actor='test')
    registry.bind_role(ConnectionRole.ANSWER,first.revision_id,expected_binding_revision=None,actor='test')
    registry.revise_connection(first.connection_id,expected_revision_id=first.revision_id,model_id='not-bound',actor='test')
    assert assigned_resource_target().model_id=='bound'
    assert assigned_resource_target().remote
    result=admission.evaluate_chat_admission(current_mode={'mode':'indexing'},metrics_cache={'ram_free_gb':3},active_jobs=1)
    assert result.allowed and result.indexing_chat_policy['connection_revision']==first.revision_id


@pytest.mark.parametrize('mode,remote,allowed',[('indexing',False,False),('indexing',True,True),('maintenance',True,False)])
def test_remote_does_not_override_maintenance(mode, remote, allowed):
    result=admission.evaluate_chat_admission(current_mode={'mode':mode},metrics_cache={'ram_free_gb':12},
        active_jobs=1,connection=target('remote' if remote else 'loopback'))
    assert result.allowed is allowed


@pytest.mark.asyncio
async def test_waiter_rechecks_memory_and_releases_only_own_slot():
    runtime=state()
    await runtime.llm_semaphore.acquire()
    async def wait():
        async with generation_guard(runtime,target()):
            pytest.fail('Memory became unsafe while queued')
    task=asyncio.create_task(wait())
    for _ in range(10): await asyncio.sleep(0)
    assert not task.done()
    runtime.metrics_cache['ram_free_gb']=1
    runtime.llm_semaphore.release()
    with pytest.raises(HTTPException) as failure: await task
    assert failure.value.status_code==503
    assert not runtime.llm_semaphore.locked()


@pytest.mark.asyncio
async def test_cancelled_waiter_does_not_release_another_request_slot():
    runtime=state()
    await runtime.llm_semaphore.acquire()
    async def wait():
        async with generation_guard(runtime,target()): pytest.fail('slot is held')
    task=asyncio.create_task(wait())
    for _ in range(10): await asyncio.sleep(0)
    task.cancel()
    with pytest.raises(asyncio.CancelledError): await task
    assert runtime.llm_semaphore.locked()
    runtime.llm_semaphore.release()


@pytest.mark.asyncio
async def test_remote_failure_cannot_bypass_local_fallback_guard():
    runtime=state(memory=3)
    primary=SimpleNamespace(locality=ConnectionLocality.REMOTE,revision_id='remote:r1')
    fallback=SimpleNamespace(locality=ConnectionLocality.LOOPBACK,revision_id='local:r1')
    class Resolver:
        def resolve(self, role): return primary
        def resolve_fallback(self, revision): return fallback
    calls=[]
    class Transport:
        async def complete(self, connection, request):
            calls.append(connection.revision_id)
            raise ModelTransportError('upstream unavailable')
    runner=BoundModelChatRunner(resolver=Resolver(),transport=Transport(),
        connection_guard=lambda connection:generation_guard(runtime,connection))
    with pytest.raises(HTTPException) as failure:
        await runner.complete(mode=CanonicalRouteMode.ACTIVE,
            request=InferenceRequest(messages=({'role':'user','content':'Test'},),max_output_tokens=20),
            legacy_complete=lambda _:pytest.fail('legacy must not run'))
    assert failure.value.status_code==503 and calls==['remote:r1']
    assert not runtime.llm_semaphore.locked()
