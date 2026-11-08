"""Compatibility client for the runtime-owned Rust model HTTP bridge."""

from __future__ import annotations

import ipaddress
import json
import os
from pathlib import Path
import struct
from urllib.parse import urlsplit
from urllib.request import getproxies, proxy_bypass

import httpx

from backend.http_client_policy import tls_verify_for_url
from proxy.services.model_connection_contracts import ConnectionLocality
from proxy.services.model_connection_security_service import ValidatedEndpoint

_LOCALHOST = frozenset(map(ipaddress.ip_address, ("127.0.0.1", "::1")))
_bridge: tuple[str, str] | None = None
_inference: tuple[str, str] | None = None
CHAT_PROTOCOL = "ollama-chat.v1"
OPENAI_CHAT_PROTOCOL = "openai-chat.v1"
EMBED_PROTOCOL = "ollama-embed.v1"
ChatLimits = tuple[int, int, bool]
EmbedLimits = tuple[int, int]


def initialize() -> None:
    """Called before dotenv or application subprocesses can see this token."""
    global _bridge, _inference
    url = os.environ.pop("LES_INTERNAL_MODEL_URL", "")
    token = os.environ.pop("LES_INTERNAL_MODEL_TOKEN", "")
    inference = os.environ.pop('LES_INTERNAL_INFERENCE_URL', '')
    if not url or not token:
        if inference:
            raise RuntimeError('Invalid internal inference configuration')
        return
    parsed = urlsplit(url)
    if (
        parsed.scheme != "http"
        or not parsed.port
        or parsed.netloc != f"127.0.0.1:{parsed.port}"
        or parsed.path != "/api/light/internal-model-http"
        or parsed.query
        or parsed.fragment
        or len(token) != 64
        or any(char not in "0123456789abcdef" for char in token)
    ):
        raise RuntimeError("Invalid internal model HTTP configuration")
    _bridge = url, token
    _inference = None
    if inference:
        expected = url.rsplit('/', 1)[0] + '/internal-inference'
        if inference != expected:
            raise RuntimeError('Invalid internal inference configuration')
        _inference = inference, token


def inference_ready() -> bool:
    return _inference is not None


def web_ready() -> bool:
    return _bridge is not None


def web_request(operation: str, arguments: dict) -> dict:
    """Local framing only. Rust owns external HTTP, DNS, limits and HTML parsing."""
    if _bridge is None:
        raise RuntimeError('Native web tools are not configured')
    url, token = _bridge
    with httpx.Client(timeout=35.0, trust_env=False) as client:
        response = client.post(url.rsplit('/', 1)[0] + '/internal-web',
            headers={'X-Les-Model-Token': token},
            json={**arguments, 'schema': 'les.web.v1', 'operation': operation})
        response.raise_for_status()
        if response.headers.get('x-les-handler') != 'rust.web':
            raise RuntimeError('WEB_RESPONSE_INVALID')
        payload = response.json()
        if not isinstance(payload, dict) or payload.get('status') not in {'ok', 'error', 'missing'}:
            raise RuntimeError('WEB_RESPONSE_INVALID')
        return payload


async def open_inference(revision_id: str, request: dict, timeout: float, *, streaming: bool,
                         require_stream: bool = False, allow_remote: bool = False) -> httpx.Response:
    if _inference is None:
        raise RuntimeError('Native inference is not configured')
    url, token = _inference
    client = httpx.AsyncClient(timeout=timeout, trust_env=False)
    try:
        outgoing = client.build_request('POST', url, headers={'X-Les-Model-Token': token}, json={
            'schema': 'les.inference.v1', 'revision_id': revision_id, 'streaming': streaming,
            'require_stream': require_stream, 'allow_remote': allow_remote,
            'timeout_seconds': max(1, min(600, int(timeout))), 'request': request,
        })
        response = await client.send(outgoing, stream=True, follow_redirects=False)
        if response.headers.get('x-les-handler') != 'rust.inference' or response.headers.get('x-les-model-protocol') != 'les.inference.v1':
            await response.aclose()
            raise RuntimeError('UPSTREAM_RESPONSE_INVALID')
    except BaseException:
        await client.aclose()
        raise
    response.stream = _OwnedStream(response.stream, client)
    return response


async def open_embeddings(revision_id, inputs, timeout, *, release_after=False, allow_remote=False,
                          response_limit=1024 * 1024):
    if _inference is None:
        raise RuntimeError('Native inference is not configured')
    url, token = _inference
    client = httpx.AsyncClient(timeout=timeout, trust_env=False)
    try:
        outgoing = client.build_request('POST', url + '/embeddings', headers={'X-Les-Model-Token': token}, json={
            'schema':'les.embedding-inference.v1','revision_id':revision_id,'inputs':list(inputs),
            'release_after':release_after,'allow_remote':allow_remote,'response_limit':response_limit,
            'timeout_seconds':max(1,min(600,int(timeout)))})
        response = await client.send(outgoing, stream=True, follow_redirects=False)
        if response.is_success and (response.headers.get('x-les-handler') != 'rust.embedding-inference'
                or response.headers.get('x-les-model-protocol') != EMBED_PROTOCOL):
            await response.aclose()
            raise RuntimeError('UPSTREAM_EMBEDDING_RESPONSE_INVALID')
    except BaseException:
        await client.aclose()
        raise
    response.stream = _OwnedStream(response.stream, client)
    return response


def bridge_for(url: str, endpoint: ValidatedEndpoint, *, has_secret: bool) -> tuple[str, str] | None:
    if _bridge is None or has_secret or endpoint.locality is not ConnectionLocality.LOOPBACK:
        return None
    parsed = urlsplit(url)
    if parsed.scheme != "http" or not (parsed.path in {"/api/chat", "/api/embed"} or parsed.path.endswith(("/embeddings", "/chat/completions"))):
        return None
    if parsed.hostname == "localhost":
        return _bridge if endpoint.allowed_addresses == _LOCALHOST else None
    try:
        address = ipaddress.ip_address(parsed.hostname)
    except ValueError:
        return None
    return _bridge if address.is_loopback and endpoint.allowed_addresses == {address} else None


def external_embedding_bridge(url: str, endpoint: ValidatedEndpoint) -> tuple[str, str] | None:
    if _bridge is None or not endpoint.allowed_addresses or len(endpoint.allowed_addresses) > 16:
        return None
    if os.getenv('SSL_CERT_FILE') or os.getenv('SSL_CERT_DIR') or os.getenv('CURL_CA_BUNDLE'):
        return None  # Keep the configured custom certificate policy.
    path = Path(os.environ.get('SystemRoot', 'C:/Windows')) / 'System32/curl.exe' if os.name == 'nt' else Path('/usr/bin/curl')
    parsed = urlsplit(url)
    if not path.is_file() or parsed.hostname != endpoint.host:
        return None
    proxies = getproxies()
    if (proxies.get(parsed.scheme) or proxies.get('all')) and not proxy_bypass(parsed.netloc):
        return None  # Keep the configured proxy policy.
    return _bridge


class _OwnedStream(httpx.AsyncByteStream):
    def __init__(self, stream: httpx.AsyncByteStream, client: httpx.AsyncClient):
        self.stream = stream
        self.client = client

    async def __aiter__(self):
        async for part in self.stream:
            yield part

    async def aclose(self) -> None:
        try:
            await self.stream.aclose()
        finally:
            await self.client.aclose()


async def open_stream(
    bridge: tuple[str, str], target: str, body: dict, timeout: float, *, chat_limits: ChatLimits | None = None,
    embed_limits: EmbedLimits | None = None,
    external: tuple[ValidatedEndpoint, str | None] | None = None,
) -> httpx.Response:
    url, token = bridge
    client = httpx.AsyncClient(timeout=timeout, trust_env=False, verify=tls_verify_for_url(url))
    try:
        headers = {"x-les-model-token": token, "x-les-model-target": target}
        if chat_limits is not None:
            tokens, limit, reasoning = chat_limits
            headers["x-les-chat-limits"] = f"{tokens}/{limit}/{int(reasoning)}"
            if urlsplit(target).path.endswith('/chat/completions'):
                headers['x-les-chat-wire'] = 'sse' if body.get('stream') else 'json'
        if embed_limits is not None:
            count, limit = embed_limits
            headers["x-les-embed-limits"] = f"{count}/{limit}"
        if external is not None:
            endpoint, authorization = external
            namespace = 'chat' if chat_limits is not None else 'embed'
            headers[f'x-les-{namespace}-addresses'] = ','.join(sorted(map(str, endpoint.allowed_addresses)))
            headers[f'x-les-{namespace}-locality'] = endpoint.locality.value
            headers[f'x-les-{namespace}-deadline'] = str(max(1, min(600, int(timeout))))
            if authorization:
                headers['Authorization'] = authorization
        request = client.build_request("POST", url, headers=headers, json=body)
        response = await client.send(request, stream=True, follow_redirects=False)
    except BaseException:
        await client.aclose()
        raise
    response.stream = _OwnedStream(response.stream, client)
    return response


def decode_embeddings(raw: bytes, count: int) -> tuple[dict, tuple[tuple[float, ...], ...]]:
    """Unpack the Rust bridge's metadata line and exact little-endian f64 rows."""
    end = raw.find(b"\n")
    if end < 0:
        raise ValueError("Missing embedding frame header")
    metadata = json.loads(raw[:end])
    if not isinstance(metadata, dict) or metadata.get("count") != count:
        raise ValueError("Unexpected embedding frame count")
    dimension = metadata.get("dimension")
    offset = end + 1
    if type(dimension) is not int or dimension < 1 or len(raw) - offset != count * dimension * 8:
        raise ValueError("Unexpected embedding frame dimensions")
    unpack = struct.Struct(f"<{dimension}d")
    return metadata, tuple(unpack.unpack_from(raw, offset + row * unpack.size) for row in range(count))
