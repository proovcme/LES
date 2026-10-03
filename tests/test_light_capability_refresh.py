import asyncio
from types import SimpleNamespace

import pytest
from fastapi import HTTPException
from proxy.services.model_capability_refresh_service import refresh_bound_capabilities
from proxy.services.model_connection_contracts import ConnectionRole
from proxy.services.model_connection_resolver_service import ModelConnectionResolutionError


@pytest.fixture
def runtime(monkeypatch):
    monkeypatch.setattr('proxy.services.chat_runtime._active_dispatcher_reindex_jobs', lambda _: 0)
    for key in ('LES_CHAT_MEMORY_GUARD', 'LES_CHAT_MIN_FREE_GB'):
        monkeypatch.delenv(key, raising=False)
    return SimpleNamespace(current_mode={'mode': 'chat'}, metrics_cache={'ram_free_gb': 12},
                           job_service=None, job_tracker={}, llm_semaphore=asyncio.Semaphore(1))


def setup(runtime):
    revision = SimpleNamespace(revision_id='answer:r1', locality='loopback')
    class Resolver:
        refreshed = False
        def __init__(self):
            self.registry = SimpleNamespace(
                get_role_binding=lambda role: SimpleNamespace(connection_revision_id=revision.revision_id)
                    if role is ConnectionRole.ANSWER else None,
                get_revision=lambda _: revision)
        def resolve(self, role):
            if not self.refreshed:
                raise ModelConnectionResolutionError('CAPABILITY_SNAPSHOT_STALE')
            return revision
    resolver = Resolver()
    calls = []
    class Probe:
        async def probe_and_store(self, actual, **kwargs):
            assert runtime.llm_semaphore.locked()
            calls.append(actual.revision_id)
            await asyncio.sleep(0)
            resolver.refreshed = True
    return resolver, Probe(), calls


@pytest.mark.asyncio
async def test_concurrent_refresh_waits_for_chat_and_probes_only_once(runtime):
    resolver, probe, calls = setup(runtime)
    await runtime.llm_semaphore.acquire()
    tasks = [asyncio.create_task(refresh_bound_capabilities(resolver=resolver, probe=probe, state=runtime))
             for _ in range(3)]
    for _ in range(10): await asyncio.sleep(0)
    assert not calls and not any(task.done() for task in tasks)
    runtime.llm_semaphore.release()
    await asyncio.gather(*tasks)
    assert calls == ['answer:r1'] and not runtime.llm_semaphore.locked()


@pytest.mark.asyncio
async def test_refresh_checks_memory_again_after_wait(runtime):
    resolver, probe, calls = setup(runtime)
    await runtime.llm_semaphore.acquire()
    task = asyncio.create_task(refresh_bound_capabilities(resolver=resolver, probe=probe, state=runtime))
    for _ in range(10): await asyncio.sleep(0)
    runtime.metrics_cache['ram_free_gb'] = 1
    runtime.llm_semaphore.release()
    with pytest.raises(HTTPException) as error: await task
    assert error.value.status_code == 503
    assert not calls and not runtime.llm_semaphore.locked()


@pytest.mark.asyncio
async def test_cancel_refresh_waiter_keeps_chat_permit_and_allows_retry(runtime):
    resolver, probe, calls = setup(runtime)
    await runtime.llm_semaphore.acquire()
    task = asyncio.create_task(refresh_bound_capabilities(resolver=resolver, probe=probe, state=runtime))
    for _ in range(10): await asyncio.sleep(0)
    task.cancel()
    with pytest.raises(asyncio.CancelledError): await task
    assert not calls and runtime.llm_semaphore.locked()
    runtime.llm_semaphore.release()
    await refresh_bound_capabilities(resolver=resolver, probe=probe, state=runtime)
    assert calls == ['answer:r1']
