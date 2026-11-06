"""Compatibility client for the runtime-owned Rust model HTTP bridge."""

from __future__ import annotations

import ipaddress
import json
import os
import struct
from urllib.parse import urlsplit

import httpx

from backend.http_client_policy import tls_verify_for_url
from proxy.services.model_connection_contracts import ConnectionLocality
from proxy.services.model_connection_security_service import ValidatedEndpoint

_LOCALHOST = frozenset(map(ipaddress.ip_address, ("127.0.0.1", "::1")))
_bridge: tuple[str, str] | None = None
CHAT_PROTOCOL = "ollama-chat.v1"
EMBED_PROTOCOL = "ollama-embed.v1"
ChatLimits = tuple[int, int, bool]
EmbedLimits = tuple[int, int]


def initialize() -> None:
    """Called before dotenv or application subprocesses can see this token."""
    global _bridge
    url = os.environ.pop("LES_INTERNAL_MODEL_URL", "")
    token = os.environ.pop("LES_INTERNAL_MODEL_TOKEN", "")
    if not url or not token:
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


def bridge_for(url: str, endpoint: ValidatedEndpoint, *, has_secret: bool) -> tuple[str, str] | None:
    if _bridge is None or has_secret or endpoint.locality is not ConnectionLocality.LOOPBACK:
        return None
    parsed = urlsplit(url)
    if parsed.scheme != "http" or parsed.path not in {"/api/chat", "/api/embed"}:
        return None
    if parsed.hostname == "localhost":
        return _bridge if endpoint.allowed_addresses == _LOCALHOST else None
    try:
        address = ipaddress.ip_address(parsed.hostname)
    except ValueError:
        return None
    return _bridge if address.is_loopback and endpoint.allowed_addresses == {address} else None


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
) -> httpx.Response:
    url, token = bridge
    client = httpx.AsyncClient(timeout=timeout, trust_env=False, verify=tls_verify_for_url(url))
    try:
        headers = {"x-les-model-token": token, "x-les-model-target": target}
        if chat_limits is not None:
            tokens, limit, reasoning = chat_limits
            headers["x-les-chat-limits"] = f"{tokens}/{limit}/{int(reasoning)}"
        if embed_limits is not None:
            count, limit = embed_limits
            headers["x-les-embed-limits"] = f"{count}/{limit}"
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
