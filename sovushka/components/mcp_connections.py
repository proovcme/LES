"""Compact MCP connection manager using the shared UI kit."""
import asyncio
from nicegui import ui
from sovushka.state import api_get, api_post, api_put, api_delete, last_api_error_text
from sovushka.uikit.components import panel, section_heading, text_field, action_button, render_feedback_state


def build_mcp_connections(on_changed):
    with ui.expansion("Внешние инструменты · MCP", icon="o_extension").classes("w-full"):
        with panel(variant="inset", classes="w-full"):
            section_heading("Подключите свой сервер", "Адрес → проверка → выбор инструментов. После подключения отметьте нужные инструменты в профиле.")
            ui.label("Поддерживаются HTTP без авторизации и локальные программы MCP. Подключайте доверенные серверы: отметку «только чтение» сообщает сам сервер.").classes("sov-ui-section-detail")
            name = text_field(label="Название", placeholder="Например, база знаний", classes="w-full")
            transport = ui.select({"http": "По адресу HTTP", "stdio": "Локальная программа"}, value="http", label="Тип подключения").classes("w-full")
            url = text_field(label="Адрес MCP", placeholder="http://localhost:8000/mcp", classes="w-full")
            url.bind_visibility_from(transport, "value", backward=lambda value: value == "http")
            program = text_field(label="Полный путь к программе MCP", placeholder="Выберите установленный сервер .exe", classes="w-full")
            program.bind_visibility_from(transport, "value", backward=lambda value: value == "stdio")
            arguments = ui.textarea(label="Аргументы запуска — по одному в строке").classes("w-full")
            arguments.bind_visibility_from(transport, "value", backward=lambda value: value == "stdio")
            container = ui.column().classes("w-full")

            async def refresh():
                result = await api_get("/api/mcp")
                container.clear()
                with container:
                    if not isinstance(result, dict):
                        render_feedback_state("error", detail=last_api_error_text("Не удалось загрузить подключения"))
                        action_button("Повторить", on_click=refresh, variant="secondary")
                        return
                    for connection in result.get("connections", []):
                        with panel(variant="raised", classes="w-full"):
                            section_heading(connection["name"], connection.get("command") or connection["url"])
                            ui.label(f"Разрешено инструментов: {len(connection['tools'])}").classes("sov-ui-section-detail")
                            with ui.row().classes("gap-2 flex-wrap"):
                                async def choose_clicked(_, item=connection):
                                    await choose(item)

                                action_button("Проверить и выбрать", on_click=choose_clicked, variant="secondary")
                                action_button("Удалить", on_click=lambda _, item=connection: confirm_remove(item), variant="danger")
                    if not result.get("connections"):
                        ui.label("Подключений пока нет. Встроенные инструменты работают без MCP.").classes("sov-ui-section-detail")

            async def add():
                add_button.disable()
                try:
                    result = await api_post("/api/mcp", {"name": name.value or "", "url": url.value or "", "transport": transport.value,
                        "command": program.value or "", "args": (arguments.value or "").splitlines()})
                    if not isinstance(result, dict):
                        ui.notify(last_api_error_text("Подключение не сохранено"), type="negative")
                        return
                    name.set_value("")
                    url.set_value("")
                    program.set_value("")
                    arguments.set_value("")
                    await refresh()
                    await choose(result)
                finally:
                    add_button.enable()

            async def choose(connection):
                dialog = ui.dialog()
                with dialog, panel(variant="raised", classes="sov-ui-dialog"):
                    section_heading(connection["name"], "Проверка только получает каталог; инструменты не вызываются.")
                    body = ui.column().classes("w-full")
                    close = action_button("Закрыть", on_click=dialog.close, variant="quiet")
                dialog.open()
                with body:
                    ui.spinner()
                result = await api_post(f"/api/mcp/{connection['id']}/probe", {})
                body.clear()
                with body:
                    if not isinstance(result, dict):
                        render_feedback_state("error", detail=last_api_error_text("Сервер недоступен; проверьте адрес"))
                        return
                    selected = {tool["name"] for tool in connection["tools"]}
                    if not result.get("tools"):
                        ui.label("Сервер не предоставил инструментов")
                    for tool in result.get("tools", []):
                        allowed = tool.get("annotations", {}).get("readOnlyHint") is True
                        checkbox = ui.checkbox(tool.get("title") or tool["name"], value=tool["name"] in selected)
                        if not allowed:
                            checkbox.disable()
                        if tool.get("description"):
                            ui.label(tool["description"]).classes("sov-ui-section-detail")
                        if not allowed:
                            ui.label("Недоступно: сервер не объявил этот инструмент как чтение").classes("sov-ui-section-detail")

                        def toggle(event, remote_name=tool["name"]):
                            (selected.add if event.value else selected.discard)(remote_name)

                        checkbox.on_value_change(toggle)

                    async def apply():
                        apply_button.disable()
                        close.disable()
                        try:
                            saved = await api_put(f"/api/mcp/{connection['id']}/tools", {"names": sorted(selected)})
                            if not isinstance(saved, dict):
                                ui.notify(last_api_error_text("Выбор не сохранён"), type="negative")
                                return
                            dialog.close()
                            await refresh()
                            await on_changed()
                        finally:
                            apply_button.enable()
                            close.enable()

                    apply_button = action_button("Разрешить выбранные", on_click=apply, variant="primary")

            def confirm_remove(connection):
                dialog = ui.dialog()
                with dialog, panel(variant="raised", classes="sov-ui-dialog"):
                    section_heading("Удалить подключение?", "Его инструменты перестанут работать в профилях. Данные на сервере сохранятся.")
                    ui.label(connection["name"])

                    async def remove():
                        delete_button.disable()
                        result = await api_delete(f"/api/mcp/{connection['id']}")
                        if not isinstance(result, dict) or result.get("status") != "deleted":
                            delete_button.enable()
                            ui.notify(last_api_error_text("Подключение не удалено"), type="negative")
                            return
                        dialog.close()
                        await refresh()
                        await on_changed()

                    delete_button = action_button("Удалить подключение", on_click=remove, variant="danger")
                    action_button("Отмена", on_click=dialog.close, variant="quiet")
                dialog.open()

            add_button = action_button("Добавить сервер", icon="o_add", on_click=add, variant="secondary")
            asyncio.create_task(refresh())
