import asyncio
import inspect
import pytest
import pytest_asyncio
from nicegui import Client, ui
from nicegui.page import page
from sovushka.components import mcp_connections as view


@pytest_asyncio.fixture(autouse=True)
async def ui_loop(monkeypatch):
    from nicegui import core
    monkeypatch.setattr(core, "loop", asyncio.get_running_loop())
    yield
    await asyncio.sleep(0)


async def click(element):
    for event in element._event_listeners.values():
        if event.type != "click":
            continue
        result = event.handler(None) if inspect.signature(event.handler).parameters else event.handler()
        if inspect.isawaitable(result):
            await result
    await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_probe_failure_keeps_close_available_and_add_form_preserved(monkeypatch):
    async def get(_):
        return {"connections": [{"id": "test", "name": "Тест", "url": "http://localhost/mcp", "tools": []}]}
    async def post(*_):
        return None
    async def changed():
        pytest.fail("Failed probe cannot change profile")
    monkeypatch.setattr(view, "api_get", get)
    monkeypatch.setattr(view, "api_post", post)
    monkeypatch.setattr(view, "last_api_error_text", lambda _: "Проверьте адрес")
    with Client(page("/__mcp_failure")) as client:
        view.build_mcp_connections(changed)
        await asyncio.sleep(0)
        inputs = [item for item in client.elements.values() if isinstance(item, ui.input)]
        inputs[0].set_value("Несохранённое название")
        inputs[1].set_value("https://example.org/mcp")
        button = next(item for item in client.elements.values() if getattr(item, "text", "") == "Проверить и выбрать")
        await click(button)
        close = next(item for item in client.elements.values() if getattr(item, "text", "") == "Закрыть")
        assert close.enabled
        assert any(getattr(item, "text", "") == "Проверьте адрес" for item in client.elements.values())
        await click(close)
        assert all(not item.value for item in client.elements.values() if isinstance(item, ui.dialog))
        assert inputs[0].value == "Несохранённое название"


@pytest.mark.asyncio
async def test_delete_cancel_does_not_call_server(monkeypatch):
    async def get(_):
        return {"connections": [{"id": "test", "name": "Тест", "url": "http://localhost/mcp", "tools": []}]}
    async def forbidden(*_):
        pytest.fail("Cancel must not delete or refresh profile")
    monkeypatch.setattr(view, "api_get", get)
    monkeypatch.setattr(view, "api_delete", forbidden)
    with Client(page("/__mcp_cancel")) as client:
        view.build_mcp_connections(forbidden)
        await asyncio.sleep(0)
        await click(next(item for item in client.elements.values() if getattr(item, "text", "") == "Удалить"))
        await click(next(item for item in client.elements.values() if getattr(item, "text", "") == "Отмена"))
        assert all(not item.value for item in client.elements.values() if isinstance(item, ui.dialog))


@pytest.mark.asyncio
async def test_profile_can_remove_a_tool_after_its_connection_was_deleted(monkeypatch):
    from sovushka.pages import profiles
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    revision = {"revision_id": "saved", "name": "Мой профиль", "prompt_revision_id": "p", "skill_revision_id": "s",
                "prompt_text": "Помогай пользователю.", "skill_text": "Отвечай понятно.", "tools": ["mcp_removed"]}
    registry = {"default_mode": "agent", "profiles": [{"mode": "agent", "active_revision_id": "saved", "revisions": [revision]}], "tools": [], "prompt_revisions": [], "skill_revisions": []}
    registry["text_limits"] = {"prompt": 16000, "skill": 8000}
    submitted = []

    async def get(_):
        return registry

    async def post(path, body):
        submitted.append((path, body))
        return {"revision_id": "new"}

    monkeypatch.setattr(profiles, "api_get", get)
    monkeypatch.setattr(profiles, "api_post", post)
    with Client(page("/__profile_missing_tool")) as client:
        profiles.build_profiles()
        await asyncio.sleep(0)
        checkbox = next(item for item in client.elements.values() if isinstance(item, ui.checkbox) and getattr(item, "text", "").startswith("Недоступный инструмент"))
        assert checkbox.value is True
        checkbox.set_value(False)
        await click(next(item for item in client.elements.values() if getattr(item, "text", "") == "Сохранить версию"))
        payload = next(body for path, body in submitted if path == "/api/profiles/revisions")
        assert payload["tools"] == []
