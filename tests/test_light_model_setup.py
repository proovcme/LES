"""Simple setup uses discovery, explicit roles and recoverable failures."""
import asyncio

import pytest
from nicegui import Client, ui
from nicegui.page import page

from proxy.routers import model_connections as router
from sovushka.components import model_setup
from tests.test_light_ui_handlers import click


@pytest.mark.asyncio
async def test_engine_discovery_is_bounded_to_loopback_and_preserves_empty_servers(monkeypatch):
    calls = []
    async def discover(request, admin):
        calls.append(request)
        assert request.secret_value is None
        if ':11434/' in request.base_url:
            return {"status": "ok", "models": ["chat", "embedding"]}
        if ':1234/' in request.base_url:
            return {"status": "ok", "models": []}
        return {"status": "unavailable"}
    monkeypatch.setattr(router, "discover_models", discover)
    result = await router.discover_local_engines(object())
    assert len(calls) == 5
    assert all(request.base_url.startswith("http://127.0.0.1:") for request in calls)
    assert [item["id"] for item in result["engines"]] == ["ollama", "lm_studio"]
    assert result["engines"][1]["models"] == []


@pytest.mark.parametrize("address, expected", [
    ("http://127.0.0.1:1234/v1", "loopback"),
    ("http://localhost:8080/v1", "loopback"),
    ("http://192.168.1.20:8080/v1", "private_network"),
    ("https://api.example.com/v1", "remote"),
    ("http://[", "remote"),
])
def test_api_locality_requires_no_user_network_settings(address, expected):
    assert model_setup.endpoint_locality(address) == expected


@pytest.mark.asyncio
@pytest.mark.parametrize("local", [True, False])
async def test_setup_discovers_then_explicitly_assigns_with_duplicate_free_retry(monkeypatch, local):
    from nicegui import core
    monkeypatch.setattr(core, "loop", asyncio.get_running_loop())
    timers, calls, completed = [], [], []
    attempts = []
    async def get(route):
        return {"connections": [], "bindings": {"answer": {"binding_revision": 4}}}
    async def post(route, payload):
        calls.append((route, payload))
        if route.endswith("discover-local"):
            return {"engines": [{"id": "studio", "name": "LM Studio", "base_url": "http://127.0.0.1:1234/v1", "models": ["model-a"], "extension_type": "lm_studio"}]}
        if route.endswith("discover-models"):
            return {"status": "ok", "models": ["model-a"]}
        return {"connection_id": "one", "revision_id": "one:r1"}
    async def put(route, payload):
        attempts.append((route, payload))
        return None if len(attempts) == 1 else {"connection_revision_id": "one:r1"}
    async def done():
        completed.append(True)
    monkeypatch.setattr(model_setup, "api_get", get)
    monkeypatch.setattr(model_setup, "api_post", post)
    monkeypatch.setattr(model_setup, "api_put", put)
    monkeypatch.setattr(ui, "timer", lambda interval, callback, **kwargs: timers.append(callback))
    with Client(page('/__setup')) as client:
        dialog = model_setup.open_model_setup(done, local=local)
        fields = {e._props.get("label"): e for e in client.elements.values() if hasattr(e, 'set_value')}
        button = lambda text: next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == text)
        connect = button("Проверить и подключить")
        assert not connect.enabled
        if local:
            assert len(timers) == 1
            await timers[0]()
        else:
            fields["Адрес API"].set_value("https://api.example.com/v1")
            fields["API-ключ (если требуется)"].set_value("synthetic-secret")
            await click(button("Получить модели"))
            assert calls[0][1]['secret_value'] == 'synthetic-secret'
        assert len(calls) == 1 and not attempts
        assert fields["Модель"].value is None and not connect.enabled
        fields["Модель"].set_value("model-a")
        await click(connect)
        assert dialog.value and not completed
        assert fields["Модель"].value == "model-a"
        assert attempts[0][1]["expected_binding_revision"] == 4
        await click(connect)
        assert completed == [True]
        assert sum(route == '/api/model-connections' for route, _ in calls) == 1
        assert attempts[-1][0].endswith('/roles/answer')
        assert fields["API-ключ (если требуется)"].value == ""
