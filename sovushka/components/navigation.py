"""Shared return controls and URL-backed tabs; switching tabs preserves widgets."""
from __future__ import annotations

import json
import os
from urllib.parse import urlsplit
from nicegui import ui
from sovushka.uikit.components import tab_name

HOME = "/qdrant-visualizer/index.html"


def svelte_chat_url():
    value = os.getenv('LES_LIGHT_SVELTE_URL', '')
    try:
        url = urlsplit(value)
        if (url.scheme == 'http' and url.hostname == '127.0.0.1' and url.port
                and url.path == '/app/' and not (url.username or url.password or url.query or url.fragment)):
            return value
    except ValueError:
        pass
    return None


def return_controls():
    with ui.element("nav").classes("les-return-controls").props('aria-label="Возврат"'):
        ui.button("Назад", icon="o_arrow_back").props('flat no-caps data-les-back aria-label="На шаг назад"')
        ui.link("Главная", target=HOME).classes("les-home-link")
        if target := svelte_chat_url():
            ui.link('Новый чат', target=target).classes('les-home-link les-chat-return-link')


def bind_route_tabs(tabs, refs, path):
    """Bind the same tab map to clicks, Back, deep links and browser history."""
    visible = {key: tab for key, tab in refs.items() if tab is not None}

    def changed(event):
        key = next((key for key, tab in visible.items() if tab_name(tab) == tab_name(event.value)), None)
        if key:
            target = f"{path}?tab={key}"
            ui.run_javascript(f"window.lesNavigation?.record({json.dumps(target)})")

    def restore(event):
        value = event.args if isinstance(event.args, dict) else {}
        if value.get("path") == path and value.get("tab") in visible:
            tabs.set_value(visible[value["tab"]])

    tabs.on_value_change(changed)
    ui.on("les-navigation", restore)
