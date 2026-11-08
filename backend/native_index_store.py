"""Compatibility codec for the Rust Qdrant boundary."""
import os
import logging
from time import perf_counter
from dataclasses import asdict
from hmac import compare_digest
from pathlib import Path
from urllib.parse import urlsplit
from uuid import uuid4

import httpx

from backend.interface import EmbeddingContractError

_bridge = None
logger = logging.getLogger(__name__)


def initialize():
    global _bridge
    endpoint = os.environ.pop("LES_NATIVE_INDEX_URL", "")
    token = os.environ.pop("LES_INTERNAL_INGEST_TOKEN", "")
    if not endpoint and not token:
        return
    url = urlsplit(endpoint)
    if (url.scheme != "http" or not url.port or url.netloc != f"127.0.0.1:{url.port}"
            or url.path != "/api/light/internal-index-store" or url.query or url.fragment
            or len(token) != 64 or any(char not in "0123456789abcdef" for char in token)):
        raise RuntimeError("Invalid internal index store configuration")
    _bridge = endpoint, token


def enabled():
    return _bridge is not None


def ensure_collection(adapter):
    """Owned transport only: Rust selects the model, probes, recovers and creates."""
    state = os.getenv('LES_STATE_ROOT')
    if _bridge is None or not state:
        raise EmbeddingContractError('INDEX_NATIVE_SERVICE_REQUIRED')
    root = Path(state).resolve()
    if (Path(adapter.db.db_path).resolve() != root/'data/meta.db'
            or adapter.content_dir.resolve() != root/'storage/datasets'
            or adapter.qdrant_url.rstrip('/') != os.getenv('LES_LIGHT_QDRANT_URL', '').rstrip('/')):
        raise EmbeddingContractError('INDEX_SETUP_SCOPE_MISMATCH')
    endpoint, token = _bridge
    with httpx.Client(timeout=120, trust_env=False) as client:
        response = client.post(endpoint.rsplit('/', 1)[0]+'/internal-index-setup',
            headers={'x-les-index-token': token}, json={'collection':adapter.collection_name})
    if not response.is_success:
        raise EmbeddingContractError(response.json().get('code', 'INDEX_SETUP_UNAVAILABLE'))
    value = response.json()
    if response.headers.get('x-les-handler') != 'rust.index-setup' or value.get('status') != 'ready':
        raise EmbeddingContractError('INDEX_SETUP_RESPONSE_INVALID')
    return value


def read_dataset_memory(dataset_id, *, background=False, force=False, max_attempts=2):
    """Local JSON transport; the native reader owns scheduling and model execution."""
    if _bridge is None:
        raise RuntimeError('READER_NATIVE_SERVICE_REQUIRED')
    endpoint, token = _bridge
    endpoint = endpoint.rsplit('/', 1)[0] + '/internal-dataset-reader'
    with httpx.Client(timeout=610 if not background else 30, trust_env=False) as client:
        response = client.post(endpoint, headers={'x-les-index-token': token},
            json={'dataset_id': str(dataset_id), 'background': bool(background),
                  'force': bool(force), 'max_attempts': max_attempts})
    if not response.is_success:
        raise RuntimeError(response.json().get('detail', 'READER_NATIVE_SERVICE_UNAVAILABLE'))
    if response.headers.get('x-les-handler') != 'rust.dataset-reader' or len(response.content) > 4 * 1024 * 1024:
        raise RuntimeError('READER_NATIVE_RESPONSE_INVALID')
    value = response.json()
    if not isinstance(value, dict):
        raise RuntimeError('READER_NATIVE_RESPONSE_INVALID')
    return value


def accepts(supplied):
    return _bridge is not None and supplied.isascii() and compare_digest(_bridge[1], supplied)


def mutate(action, collection, **fields):
    if _bridge is None:
        return False
    endpoint, token = _bridge
    try:
        with httpx.Client(timeout=65.0, trust_env=False) as client:
            response = client.post(
                endpoint, json={"action": action, "collection": collection, **fields},
                headers={"x-les-index-token": token},
            )
        if response.status_code == 200 and response.json().get("status") == "completed":
            return True
    except (httpx.HTTPError, ValueError) as error:
        raise EmbeddingContractError("INDEX_STORAGE_WRITE_FAILED") from error
    raise EmbeddingContractError("INDEX_STORAGE_WRITE_FAILED")


def upsert(client, collection, points):
    if enabled() and mutate(
        "upsert", collection, points=[point.model_dump(mode="json") for point in points],
    ):
        return
    client.upsert(collection_name=collection, points=points, wait=True)


def publication_directory(adapter):
    state = os.getenv("LES_STATE_ROOT")
    if not enabled() or not state:
        return None
    from contextlib import closing
    from proxy.services.lexical_index_service import LexicalIndex, lexical_db_path

    root = Path(state).resolve()
    database = root / "data/meta.db"
    if (Path(adapter.db.db_path).resolve() != database
            or Path(lexical_db_path()).resolve() != database
            or adapter.content_dir.resolve() != root / "storage/datasets"
            or adapter.qdrant_url.rstrip('/') != os.getenv("LES_LIGHT_QDRANT_URL", "").rstrip('/')):
        return None
    with closing(LexicalIndex(str(database)).connect()):
        pass  # Initialize or migrate the legacy FTS schema before the first write.
    directory = root / "data/index-publish"
    directory.mkdir(exist_ok=True)
    return directory


def publish_file(directory, collection, dataset_id, file_name, db_file_name,
                 registration, fingerprint, route, nodes, embed, *,
                 embedding, embedding_fingerprint, vector_names, embed_batch, batch_size,
                 cache_enabled, verify_count, before_publish):
    from backend import native_core

    spool_id = uuid4().hex
    spool = directory / f"{spool_id}.jsonl"
    cache_path = directory / f"{spool_id}.cache"
    owned_cache = owned_spool = False
    dimension = int(embedding["vector_size"])
    cache_start = perf_counter()
    try:
        if cache_enabled:
            try:
                publication_request("prepare_cache", collection, dataset_id=dataset_id,
                    file_name=file_name, embedding_fingerprint=embedding_fingerprint,
                    dense=vector_names["dense"] if vector_names else None,
                    spool_id=spool_id, dimension=dimension)
                owned_cache = True
            except EmbeddingContractError:
                logger.warning("Cached vectors unavailable for %s. Recomputing embeddings.", file_name)
        cache_sec = perf_counter() - cache_start
        prepared = native_core.prepare_index_points({
            "spool": str(spool), "count": len(nodes), "batch_size": embed_batch,
            "dimension": dimension, "vector_names": vector_names,
            "dataset_id": dataset_id, "file_name": file_name, "source_sha256": fingerprint["file_hash"],
            "embedding": embedding, "embedding_fingerprint": embedding_fingerprint,
            "cache": str(cache_path) if owned_cache else None,
        }, nodes, embed)
        owned_spool = True
        if prepared["cache_read_failed"]:
            logger.warning("Cached vectors unreadable for %s. Embeddings were recomputed.", file_name)
        before_publish()
        start = perf_counter()
        route_fields = asdict(route)
        route_fields.pop("metadata")
        result = publication_request("publish_file", collection, dataset_id=dataset_id,
            doc_name=file_name, db_file_name=db_file_name, spool_id=spool_id,
            registration=registration, fingerprint=fingerprint, route=route_fields,
            ids=prepared["ids"], batch_size=batch_size, verify_count=verify_count)
        count = len(prepared["ids"])
        if result.get("points") != count:
            raise EmbeddingContractError("INDEX_STORAGE_WRITE_FAILED")
        return {**prepared, "count": count, "cache_sec": cache_sec, "upsert_sec": perf_counter() - start}
    finally:
        for path, owned in ((cache_path, owned_cache), (spool, owned_spool)):
            if owned:
                try:
                    path.unlink(missing_ok=True)
                except OSError:
                    logger.warning("Index spool could not be removed: %s", path.name)


def publication_request(action, collection, **fields):
    endpoint, token = _bridge
    try:
        with httpx.Client(timeout=1800, trust_env=False) as client:
            response = client.post(endpoint, headers={"x-les-index-token": token},
                json={"action": action, "collection": collection, **fields})
        result = response.json()
    except (httpx.HTTPError, ValueError) as error:
        raise EmbeddingContractError("INDEX_STORAGE_WRITE_FAILED") from error
    if isinstance(result, dict):
        if response.status_code == 200:
            return result
        code = result.get("detail")
        if isinstance(code, str) and code.isascii() and code.isupper() and len(code) <= 64:
            raise EmbeddingContractError(code)
    raise EmbeddingContractError("INDEX_STORAGE_WRITE_FAILED")


def execute_batch(request):
    """Transport only; a failed native owner never falls back to Python processing."""
    if _bridge is None:
        raise EmbeddingContractError('INDEX_NATIVE_API_REQUIRED')
    endpoint, token = _bridge
    endpoint = endpoint.rsplit('/',1)[0] + '/internal-index-batch'
    try:
        with httpx.Client(timeout=3600,trust_env=False) as client:
            response=client.post(endpoint,json=request,headers={'x-les-index-token':token})
        result=response.json()
    except (httpx.HTTPError,ValueError) as error:
        # The native API owns the durable lease and may continue after disconnect.
        # Do not mark documents failed or start a duplicate local coordinator.
        raise EmbeddingContractError('INDEX_BATCH_TRANSPORT_INTERRUPTED') from error
    if (response.status_code==200 and response.headers.get('x-les-handler')=='rust.index-batch'
            and isinstance(result,dict) and result.get('owner')=='rust.index-batch'):
        return result
    code=result.get('detail') if isinstance(result,dict) else None
    if isinstance(code,str) and code.isascii() and code.replace('_','').isalnum() and code.isupper() and len(code)<=120:
        raise EmbeddingContractError(code)
    raise EmbeddingContractError('INDEX_NATIVE_API_REQUIRED')


async def read_sections(collection, seeds, scopes):
    endpoint, token = _bridge
    try:
        async with httpx.AsyncClient(timeout=14.5, trust_env=False) as client:
            response = await client.post(endpoint, headers={"x-les-index-token": token},
                json={"action": "read_sections", "collection": collection, "seeds": seeds, "scopes": scopes})
        body = response.json()
    except (httpx.HTTPError, ValueError) as error:
        raise OSError("SECTION_READ_UNAVAILABLE") from error
    code = body.get("detail") if isinstance(body, dict) else None
    if response.status_code == 409 and code in {"INDEX_CHANGED_DURING_SEARCH", "SECTION_SCOPE_MISMATCH"}:
        raise EmbeddingContractError(code)
    if response.status_code == 422 and code == "SECTION_READ_LIMIT":
        return None
    if not response.is_success or not isinstance(body, dict):
        raise OSError("SECTION_READ_UNAVAILABLE")
    return body


async def hybrid_search(collection, **fields):
    """Wire codec only: the native owner performs both retrieval legs and fusion."""
    endpoint, token = _bridge
    try:
        async with httpx.AsyncClient(timeout=120.0, trust_env=False) as client:
            response = await client.post(endpoint, headers={'x-les-index-token': token},
                json={'action': 'hybrid_search', 'collection': collection, **fields})
        body = response.json()
    except (httpx.HTTPError, ValueError) as error:
        raise EmbeddingContractError('INDEX_SEARCH_UNAVAILABLE') from error
    if not response.is_success or not isinstance(body, dict) or not isinstance(body.get('points'), list):
        raise EmbeddingContractError(body.get('detail', 'INDEX_SEARCH_UNAVAILABLE') if isinstance(body, dict) else 'INDEX_SEARCH_UNAVAILABLE')
    return body
