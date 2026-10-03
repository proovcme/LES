from types import SimpleNamespace

import pytest

from backend import system_memory, metrics_collector
from proxy.routers import runtime


@pytest.mark.asyncio
async def test_memory_uses_this_machine_and_real_swap_despite_legacy_host(monkeypatch):
    monkeypatch.setenv('LES_LLM_PROVIDER', 'mlx')
    monkeypatch.setenv('MLX_URL', 'http://unrelated-host.invalid')
    monkeypatch.setattr(system_memory.psutil, 'virtual_memory',
                        lambda: SimpleNamespace(total=32e9, used=27e9, available=5e9))
    monkeypatch.setattr(system_memory.psutil, 'swap_memory',
                        lambda: SimpleNamespace(total=10e9, used=9e9, percent=90))
    monkeypatch.setattr(runtime.httpx, 'AsyncClient',
                        lambda **_: pytest.fail('Memory must not query a model host'))
    result = await runtime._host_memory()
    assert result['ram_free_gb'] == 5
    assert result['ram_total_gb'] == 32
    assert result['swap_used_gb'] == 9
    assert result['swap_pct'] == 90
    assert result['source'] == 'local_os'


@pytest.mark.asyncio
async def test_health_probe_uses_owned_runtime_port(monkeypatch):
    monkeypatch.setenv('LES_PROXY_URL', 'http://127.0.0.1:54913/')
    requested = []

    class Client:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            pass

        async def get(self, url):
            requested.append(url)
            return SimpleNamespace(status_code=200)

    monkeypatch.setattr(metrics_collector.httpx, 'AsyncClient', lambda **_: Client())
    assert await metrics_collector._get_network_ok() == 1
    assert requested == ['http://127.0.0.1:54913/api/health']


@pytest.mark.asyncio
async def test_no_owned_runtime_url_means_no_foreign_probe(monkeypatch):
    monkeypatch.delenv('LES_PROXY_URL', raising=False)
    monkeypatch.delenv('PROXY_URL', raising=False)
    monkeypatch.setattr(metrics_collector.httpx, 'AsyncClient',
                        lambda **_: pytest.fail('No configured runtime'))
    assert await metrics_collector._get_network_ok() == 0
