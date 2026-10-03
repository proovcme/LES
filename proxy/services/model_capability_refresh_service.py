"""Refresh expired role capabilities under the same resource guard as chat."""
import asyncio
import logging
from weakref import WeakValueDictionary

from proxy.services.generation_guard_service import generation_guard
from proxy.services.model_connection_contracts import CapabilityName, ConnectionRole
from proxy.services.model_connection_resolver_service import ModelConnectionResolutionError

logger = logging.getLogger(__name__)
_locks = WeakValueDictionary()
_EXPIRED = {"CAPABILITY_SNAPSHOT_MISSING", "CAPABILITY_SNAPSHOT_STALE"}
_REQUESTED = frozenset({CapabilityName.MODELS, CapabilityName.CHAT_COMPLETIONS,
                        CapabilityName.STREAMING, CapabilityName.TOOLS,
                        CapabilityName.STRUCTURED_OUTPUT})


def _needs_refresh(resolver, role):
    try:
        resolver.resolve(role)
        return False
    except ModelConnectionResolutionError as error:
        if str(error) not in _EXPIRED:
            raise
        return True


async def refresh_bound_capabilities(*, resolver, probe, state):
    registry = getattr(resolver, "registry", None)
    if registry is None:
        return
    for role in (ConnectionRole.ANSWER, ConnectionRole.LOCAL_FALLBACK):
        binding = registry.get_role_binding(role)
        if binding is None or not _needs_refresh(resolver, role):
            continue
        revision = registry.get_revision(binding.connection_revision_id)
        # Weak references keep only locks with live waiters/owners. No growing
        # global registry, and asyncio locks are never shared between event loops.
        key = (asyncio.get_running_loop(), revision.revision_id)
        lock = _locks.setdefault(key, asyncio.Lock())
        try:
            async with lock:
                current = registry.get_role_binding(role)
                if current is None or current.connection_revision_id != revision.revision_id:
                    raise ModelConnectionResolutionError("ROLE_BINDING_CHANGED")
                if not _needs_refresh(resolver, role):
                    continue
                async with generation_guard(state, revision):
                    await probe.probe_and_store(revision, requested=_REQUESTED,
                                                registry=registry, actor="system:chat-capability-refresh")
                resolver.resolve(role)
        except Exception:
            if role is ConnectionRole.ANSWER:
                raise
            logger.warning("Optional fallback capability refresh failed", exc_info=True)
