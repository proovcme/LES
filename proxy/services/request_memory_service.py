"""Pressure-only disposal of a model actually invoked by this request."""
import asyncio
from contextlib import asynccontextmanager, nullcontext
from contextvars import ContextVar
import logging

import httpx
from backend import native_core
from proxy.services.runtime_admission import (
    acquire_generation_slot, GenerationSlotTimeout, chat_min_free_gb,
)

logger = logging.getLogger(__name__)
_owned = ContextVar('les_request_owned_model', default=None)
_scope_owner = ContextVar('les_memory_scope_owner', default=None)


def remember_invocation(connection):
    _owned.set(connection)
    return _scope_owner.get() is asyncio.current_task()


def local_ollama_endpoint(connection):
    from proxy.services.model_resource_service import local_ollama_native_url
    return local_ollama_native_url(connection, 'generate')


async def release_after_request(connection, state, token_sink=None, *, permit_held=False, phase='chat_end'):
    from backend.runtime_memory import collect_transient, wait_for_chat_memory
    from proxy.services.operation_progress_service import chat_progress

    endpoint = local_ollama_endpoint(connection) if connection is not None else None
    if endpoint is None:
        return {'released': False}
    sample = await asyncio.to_thread(collect_transient, 'chat_end')
    plan = native_core.required('memory', request={
        'action': 'plan', 'phase': phase,
        'free_gb': sample['free_after_gb'], 'required_gb': chat_min_free_gb(),
        'owned_local_ollama': phase == 'chat_end',
        'planned_local_ollama': phase == 'chat_prepare',
    })
    if not plan['release_model']:
        return {'released': False}
    # Hold the normal permit so another LES generation cannot be unloaded.
    permit = (nullcontext() if permit_held else
              acquire_generation_slot(state.llm_semaphore, timeout_seconds=.01))
    try:
        async with permit:
            await chat_progress(token_sink, 'memory', 'Освобождаю память модели')
            async with httpx.AsyncClient(timeout=8, trust_env=False, follow_redirects=False) as client:
                async with client.stream('POST', endpoint, json={
                    'model': connection.model_id, 'keep_alive': 0,
                }) as reply:
                    reply.raise_for_status()
            # Unmapping is asynchronous. Admission will still sample the OS.
            await wait_for_chat_memory()
            if _owned.get() is connection:
                _owned.set(None)
            logger.info('Released request model under pressure phase=%s revision=%s',
                        phase, connection.revision_id)
            return {'released': True}
    except GenerationSlotTimeout:
        return {'released': False, 'busy': True}


async def prepare_assigned_model(state, token_sink=None):
    """Recover residency left by an earlier run, using the validated answer role only."""
    from proxy.services.chat_inference_service import _model_connection_resolver
    from proxy.services.model_connection_contracts import ConnectionRole, CapabilityName

    try:
        resolver, _ = _model_connection_resolver()
        binding = resolver.registry.get_role_binding(ConnectionRole.ANSWER)
        if binding is None:
            return {'released': False}
        revision = resolver.registry.get_revision(binding.connection_revision_id)
        # Reject remote/auth/custom endpoints before resolving any secret status.
        if local_ollama_endpoint(revision) is None:
            return {'released': False}
        connection = resolver.resolve_revision(
            binding.connection_revision_id,
            required_capabilities=frozenset({CapabilityName.CHAT_COMPLETIONS}),
        )
        return await release_after_request(connection, state, token_sink, phase='chat_prepare')
    except Exception as error:
        logger.warning('Assigned model preparation unavailable: %s', type(error).__name__)
        return {'released': False}


@asynccontextmanager
async def managed_request_memory(token_sink=None):
    token = _owned.set(None)
    owner = _scope_owner.set(asyncio.current_task())
    try:
        yield
    finally:
        connection = _owned.get()
        try:
            if connection is not None:
                from proxy.services.chat_runtime import get_chat_state
                await release_after_request(connection, get_chat_state(), token_sink)
        except Exception as error:
            # Cleanup failure cannot turn a completed answer into an error.
            logger.warning('Post-answer cleanup failed: %s', type(error).__name__)
        finally:
            _owned.reset(token)
            _scope_owner.reset(owner)


async def settle_transient_memory(token_sink=None):
    """Allow a completed import's unmapping to settle without changing admission."""
    from backend.runtime_memory import collect_transient, wait_for_chat_memory
    from proxy.services.operation_progress_service import chat_progress

    await chat_progress(token_sink, 'memory', 'Освобождаю память перед ответом')
    try:
        await asyncio.to_thread(collect_transient, 'queue_end')
        await wait_for_chat_memory(max_seconds=2, minimum_seconds=.5)
    except Exception as error:
        logger.warning('Preflight cleanup unavailable: %s', type(error).__name__)
        # The caller rechecks the original admission policy even when cleanup fails.
