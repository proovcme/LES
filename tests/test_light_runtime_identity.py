"""Status and traces must not identify an unassigned legacy engine."""
import asyncio
from types import SimpleNamespace

import pytest

from proxy.routers import runtime
from proxy.services import model_connection_registry_service as registry_module
from proxy.services.chat_request_service import _version_stamp
from proxy.services.model_connection_contracts import ConnectionLocality, ConnectionRole


@pytest.fixture
def registry(tmp_path, monkeypatch):
    value = registry_module.ModelConnectionRegistry(tmp_path / 'models.db')
    monkeypatch.setattr(registry_module, 'ModelConnectionRegistry', lambda: value)
    monkeypatch.setenv('LES_LLM_PROVIDER', 'mlx')
    monkeypatch.setenv('LLM_MODEL', 'wrong-legacy-model')
    return value


def test_unassigned_role_does_not_advertise_environment_model(registry):
    assert runtime._provider_status() == {'provider': 'unassigned', 'base_url': '', 'model': ''}
    stamp = _version_stamp()
    assert stamp['llm_model'] == 'unknown'
    assert 'norm_base' not in stamp


def test_status_describes_bound_revision_even_after_connection_edit(registry):
    first = registry.create_connection(
        display_name='Synthetic', base_url='http://127.0.0.1:11434/v1',
        model_id='selected-model', locality=ConnectionLocality.LOOPBACK,
        requested_context_tokens=None, secret_ref=None, extension_type='ollama', actor='test',
    )
    registry.bind_role(ConnectionRole.ANSWER, first.revision_id, expected_binding_revision=None, actor='test')
    registry.revise_connection(first.connection_id, expected_revision_id=first.revision_id,
                               model_id='not-yet-bound', actor='test')
    assert runtime._provider_status() == {
        'provider': 'ollama', 'base_url': first.base_url, 'model': 'selected-model',
    }


@pytest.mark.asyncio
async def test_status_never_probes_legacy_engine(registry, monkeypatch):
    state = SimpleNamespace(metrics_cache={}, current_mode={}, proxy_start=0)
    admission = SimpleNamespace(allowed=True, reason='', memory={}, payload=lambda: {})
    monkeypatch.setattr(runtime, 'get_runtime_state', lambda: state)
    monkeypatch.setattr(runtime, 'chat_admission_for_state', lambda _: admission)
    monkeypatch.setattr(runtime, 'docker_control_enabled', lambda: False)
    monkeypatch.setattr(runtime.httpx, 'AsyncClient', lambda **_: pytest.fail('status contacted a legacy engine'))
    response = await runtime.get_status()
    assert response['proxy']['llm_model'] == ''
    assert response['mlx']['status'] == 'not_probed'


@pytest.mark.parametrize('cached,live,allowed', [(1.0, 6.0, True), (12.0, 3.0, False)])
def test_ready_status_and_memory_share_live_sample_in_both_directions(monkeypatch, cached, live, allowed):
    from proxy.services import model_resource_service
    from proxy.services.model_resource_service import ModelResourceTarget
    state = SimpleNamespace(
        metrics_cache={'ram_free_gb': cached, 'swap_pct': 0}, current_mode={'mode': 'chat'},
        job_service=SimpleNamespace(list_active_ids=lambda **_: []), job_tracker={}, proxy_start=0,
    )
    samples = []
    def sample(cache):
        assert cache is state.metrics_cache
        samples.append(True)
        return {'ram_free_gb': live, 'swap_pct': 0}
    monkeypatch.setattr(runtime, 'get_runtime_state', lambda: state)
    monkeypatch.setattr(runtime, 'live_memory_metrics', sample)
    monkeypatch.setattr(runtime, 'dispatcher_for_state', lambda _: SimpleNamespace(reindex_status_payload=lambda: {'running': False}))
    monkeypatch.setattr(runtime, 'docker_control_enabled', lambda: False)
    monkeypatch.setattr(runtime, '_provider_status', lambda: {'provider': 'ollama', 'base_url': '', 'model': 'explicit'})
    monkeypatch.setattr(model_resource_service, 'assigned_resource_target', lambda: ModelResourceTarget('ollama', 'explicit', 'fixed:r1', 'loopback'))
    monkeypatch.delenv('LES_CHAT_MIN_FREE_GB', raising=False)
    monkeypatch.delenv('LES_CHAT_MEMORY_GUARD', raising=False)
    async def run():
        mode = await runtime.get_indexing_mode()
        status = await runtime.get_status()
        assert mode['chat_generation_allowed'] is allowed
        for payload in [mode, status]:
            assert payload['chat_admission']['allowed'] is allowed
            assert payload['chat_admission']['memory']['ram_free_gb'] == live
            assert payload['memory_state']['memory']['ram_free_gb'] == live
    asyncio.run(run())
    assert len(samples) == 2
    assert state.metrics_cache['ram_free_gb'] == cached
