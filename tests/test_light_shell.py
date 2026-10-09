"""The Light workspace uses its own forest navigation, not the full LES header."""
from pathlib import Path
from types import SimpleNamespace
from xml.etree import ElementTree

from nicegui import Client, app, ui
from nicegui.page import page

from sovushka.components.light_shell import _LIGHT_SHELL_CSS, build_light_shell


def test_light_shell_has_real_destinations_and_remembers_panel(monkeypatch):
    stored = {}
    monkeypatch.setattr(type(app.storage), "user", property(lambda _: stored))
    with Client(page("/__light_shell")) as client:
        tabs, refs = build_light_shell()
        assert set(refs) == {"chat", "data", "mail", "history"}
        assert {tab._props.get("label") for tab in refs.values()} == {
            "Чат", "Данные", "Почта", "История"
        }
        links = [item for item in client.elements.values() if isinstance(item, ui.link)]
        assert any(item._props.get("href") == "/qdrant-visualizer/index.html" for item in links)
        assert any(item._props.get("href") == "/les/classic?tab=models" for item in links)
        assert not any("Сметы" in str(getattr(item, "text", "")) for item in client.elements.values())
        assert not any("Найти в лесу" in str(getattr(item, "text", "")) for item in client.elements.values())
        assert any(item._props.get("aria-label") == "Ещё разделы" for item in client.elements.values())
        assert any(item._props.get("data-les-back") is True for item in client.elements.values())
        tabs.set_value(refs["data"])
        assert stored["last_chat_tab"] == "Данные"


def test_light_forest_art_is_bundled_and_chat_text_stays_above_it():
    art = Path("qdrant_visualizer/forest-mist.svg")
    assert ElementTree.parse(art).getroot().tag.endswith("svg")
    assert "/qdrant-visualizer/forest-mist.svg" in _LIGHT_SHELL_CSS
    assert ".sov-chat-empty::before" in _LIGHT_SHELL_CSS
    assert "padding:270px" not in _LIGHT_SHELL_CSS


def test_light_project_sidebar_does_not_repeat_global_navigation(monkeypatch):
    from sovushka.components import chat_project_navigation as navigation

    monkeypatch.setattr(navigation, "is_light", lambda: True)
    workspace = SimpleNamespace(is_admin=True)
    with Client(page("/__light_project_navigation")) as client:
        nav = navigation.ChatProjectNavigation(
            workspace, on_new=lambda: None, on_history=lambda: None,
            on_data=lambda: None,
        )
        nav.render()
        labels = [item.text for item in client.elements.values() if isinstance(item, ui.button)]
    assert "Новый чат" in labels
    assert "Создать проект" in labels
    assert not {"Данные", "История чатов", "Настройки"} & set(labels)


def test_projects_open_on_demand_and_reopen_after_native_dismissal(monkeypatch):
    from sovushka.components import chat_project_navigation as navigation

    monkeypatch.setattr(navigation, "is_light", lambda: True)
    with Client(page("/__project_drawer")):
        nav = navigation.ChatProjectNavigation(
            SimpleNamespace(is_admin=True), on_new=None, on_history=None,
        )
        nav.render()
        assert not nav.drawer.value
        assert nav.drawer._props["position"] == "left"
        assert nav.sidebar._props["aria-label"] == "Проекты и чаты"
        nav.toggle()
        assert nav.drawer.value
        # Escape/backdrop update the dialog value, without calling our close().
        nav.drawer.set_value(False)
        nav.toggle()
        assert nav.drawer.value
        nav.close()
        assert not nav.drawer.value
