"""Shared proxy-environment policy for internal HTTP clients.

Loopback services are part of one LES installation and must never be routed
through HTTP(S)/ALL_PROXY inherited from the desktop or service account.
External URLs keep httpx's normal environment behavior.
"""

from __future__ import annotations

import ipaddress
from functools import lru_cache
import ssl
from urllib.parse import urlsplit


def is_loopback_url(url: str) -> bool:
    """Return True only for localhost names and loopback IP addresses."""
    try:
        host = (urlsplit(str(url or "")).hostname or "").rstrip(".").lower()
    except ValueError:
        return False
    if not host:
        return False
    if host == "localhost" or host.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def trust_env_for_url(url: str) -> bool:
    """Preserve proxy env for external URLs and bypass it for loopback only."""
    return not is_loopback_url(url)


@lru_cache(maxsize=1)
def _local_tls_context() -> ssl.SSLContext:
    # httpx builds this even for plain HTTP. Loading the trust store repeatedly
    # blocks NiceGUI's event loop on Windows; share only immutable TLS settings,
    # never cookies, credentials, clients or connections between users.
    import httpx
    return httpx.create_ssl_context(trust_env=False)


def tls_verify_for_url(url: str) -> ssl.SSLContext | bool:
    """Reuse the verified trust store locally; preserve external env semantics."""
    return _local_tls_context() if is_loopback_url(url) else True
