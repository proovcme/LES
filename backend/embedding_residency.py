"""A parse call owns the embeddings it requested, never the Ollama server."""
from contextvars import ContextVar
import logging
import httpx
from proxy.services.model_resource_service import local_ollama_native_url

logger = logging.getLogger(__name__)
_scope = ContextVar('les_index_embedding_scope', default=None)


def begin():
    owned = {}
    return _scope.set(owned), owned


def end(token):
    _scope.reset(token)


def remember(connection):
    owned = _scope.get()
    if owned is None:
        return
    snapshot = getattr(connection, 'capability_snapshot', None)
    options = getattr(snapshot, 'transport_options', {})
    if (local_ollama_native_url(connection, 'embed') is not None
            and options.get('embedding_protocol') == 'native_embedding_v1'):
        owned[(connection.revision_id, connection.model_id)] = connection


async def release(owned):
    count = failed = 0
    for connection in owned.values():
        try:
            endpoint = local_ollama_native_url(connection, 'embed')
            async with httpx.AsyncClient(timeout=8, trust_env=False, follow_redirects=False) as client:
                async with client.stream('POST', endpoint, json={
                    'model': connection.model_id, 'input': [], 'keep_alive': 0,
                }) as reply:
                    reply.raise_for_status()
            count += 1
            logger.info('Released index-owned embedding model revision=%s', connection.revision_id)
        except Exception as error:
            failed += 1
            logger.warning('Embedding release failed revision=%s type=%s',
                           connection.revision_id, type(error).__name__)
    if count or failed:
        from backend.runtime_memory import wait_for_chat_memory
        await wait_for_chat_memory()
    owned.clear()
    return {'released': count, 'failed': failed}
