"""Compatibility API; trusted-network decisions belong to the shared Rust owner."""
from __future__ import annotations
from typing import Optional, Sequence
from backend import native_core


def _call(action, networks, **values):
    return native_core.required('trust', request={
        'schema': 'les.trust.v1', 'action': action, 'networks': list(networks), **values,
    })['value']


def ip_in_networks(ip_value: str, networks: Sequence[str]) -> bool:
    return _call('ip_in_networks', networks, ip=ip_value)


def resolve_client_ip(peer_ip: str, forwarded: Optional[str], real_ip: Optional[str],
                      trusted_proxy_networks: Sequence[str]) -> str:
    return _call('resolve_client_ip', trusted_proxy_networks, peer=peer_ip, forwarded=forwarded, real=real_ip)


def proxy_asserts_network(peer_ip: str, header_value: str, trusted_proxy_networks: Sequence[str],
                          trusted_role: str) -> bool:
    return _call('proxy_asserts_network', trusted_proxy_networks, peer=peer_ip, header=header_value or '', role=trusted_role)


def network_role(ip_value: str, trusted_networks: Sequence[str], trusted_role: str) -> Optional[str]:
    return _call('network_role', trusted_networks, ip=ip_value, role=trusted_role)
