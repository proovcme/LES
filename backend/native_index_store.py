"""Compatibility codec for the Rust Qdrant boundary."""
import os
from hmac import compare_digest
from urllib.parse import urlsplit

import httpx

from backend.interface import EmbeddingContractError

_bridge = None


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
