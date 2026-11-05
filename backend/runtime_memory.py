"""Collect transient objects before sampling; native owner releases only idle work."""
import gc
import asyncio
import logging
import time
from backend import native_core
logger = logging.getLogger(__name__)


async def wait_for_chat_memory(*, max_seconds=5.0, minimum_seconds=1.0):
    """Wait for OS unmapping to settle; a transient threshold crossing is not ready."""
    from backend.system_memory import system_memory_snapshot
    from proxy.services.runtime_admission import chat_min_free_gb

    started = time.monotonic()
    healthy = 0
    while True:
        free = system_memory_snapshot()['ram_free_gb']
        healthy = healthy + 1 if free >= chat_min_free_gb() else 0
        elapsed = time.monotonic() - started
        ready = healthy >= 4 and elapsed >= minimum_seconds
        if ready or elapsed >= max_seconds:
            logger.info('Memory unmapping settled=%s seconds=%.3f free_gb=%.3f',
                        ready, elapsed, free)
            return {'ready': ready, 'free_gb': free, 'seconds': elapsed}
        await asyncio.sleep(min(.25, max_seconds - elapsed))

def collect_transient(phase='batch_end'):
    from backend.system_memory import system_memory_snapshot
    from proxy.services.runtime_admission import chat_min_free_gb
    before=system_memory_snapshot()["ram_free_gb"]
    collected=gc.collect()
    result={'collected':collected,'free_before_gb':before}
    try:
        if native_core.supports('memory'):
            plan=native_core.required('memory',request={'action':'plan','phase':phase,'free_gb':before,'required_gb':chat_min_free_gb(),'owned_local_ollama':False})
            result['native']=native_core.required('memory',request={'action':'reclaim','release_worker':plan['release_worker']})
    except (native_core.NativeCoreError,OSError) as error:
        logger.warning('Transient memory reclamation unavailable: %s',type(error).__name__)
    result['free_after_gb']=system_memory_snapshot()['ram_free_gb']
    logger.info('Memory reclaimed phase=%s before_gb=%.3f after_gb=%.3f objects=%s native=%s',phase,before,result['free_after_gb'],collected,result.get('native'))
    return result
