"""Project notes and conversation context editor, separate from navigation."""
from nicegui import ui
from sovushka.state import api_get, api_post, api_patch, api_delete
from sovushka.uikit import action_button, checkbox_field, panel, section_heading, select_field, text_field

def _rows(payload, key):
    return payload.get(key, []) if isinstance(payload, dict) else list(payload or [])

async def open_workspace_memory(workspace):
    if workspace._memory_dialog and workspace._memory_dialog.value:
        return
    pid = int(workspace.active.get("project_id") or 0)
    with ui.dialog() as dialog, panel(variant="raised", classes="sov-workspace-dialog"):
        workspace._memory_dialog = dialog
        section_heading("Лес помнит", "Здесь видно, что сохраняется для продолжения работы.")
        from sovushka.components.conversation_memory import build_conversation_memory
        await build_conversation_memory(workspace.get_session_id())
        ui.separator()
        section_heading("Ваши записи", "Общие предпочтения и память проекта сохраняются по вашему действию. Для ответа рассматриваются до 24 включённых записей; при нехватке контекста часть может не войти в запрос.")
        options = {0: "Общие предпочтения"}
        if pid:
            options[pid] = "Память этого проекта"
        scope = select_field(options, value=pid, label="Где хранить", classes="w-full")
        body = ui.column().classes("w-full gap-2")

        async def refresh():
            selected = int(scope.value or 0)
            payload = await api_get(f"/api/workspace/memory?project_id={selected}")
            body.clear()
            with body:
                if payload is None:
                    ui.label("Не удалось загрузить память. Закройте окно и попробуйте снова.")
                    return
                records = _rows(payload, "notes")
                if not records:
                    ui.label("Здесь пока нет записей.").classes("sov-muted")
                for note in records:
                    with panel(variant="inset", classes="w-full"):
                        if note.get("auto"):
                            ui.label("Историческая автозаметка. Сохраните явно, чтобы использовать в новых чатах.").classes("sov-muted")
                        field = text_field(label="Запись", value=note["text"], classes="w-full").props("type=textarea autogrow maxlength=2000")
                        enabled = checkbox_field("Использовать в ответах", value=bool(note.get("enabled", True)))

                        async def save_note(nid=note["id"], text=field, flag=enabled, owner=selected):
                            value = str(text.value or "").strip()
                            if not value:
                                ui.notify("Запись не может быть пустой", type="warning")
                                return
                            result = await api_patch(f"/api/workspace/memory/{nid}", {
                                "project_id": owner, "text": value, "enabled": bool(flag.value),
                            })
                            if result is None:
                                ui.notify("Не удалось сохранить запись", type="negative")
                            else:
                                ui.notify("Запись сохранена")

                        async def delete_note(nid=note["id"], owner=selected):
                            result = await api_delete(f"/api/workspace/memory/{nid}?project_id={owner}")
                            if result is None:
                                ui.notify("Не удалось удалить запись", type="negative")
                            else:
                                await refresh()

                        with ui.row().classes("gap-2"):
                            action_button("Сохранить", on_click=save_note)
                            action_button("Удалить", on_click=delete_note, variant="danger")

        scope.on_value_change(refresh)
        new_note = text_field(label="Что запомнить", classes="w-full").props("type=textarea autogrow maxlength=2000")

        async def remember():
            value = str(new_note.value or "").strip()
            if not value:
                ui.notify("Введите запись для памяти", type="warning")
                return
            selected = int(scope.value or 0)
            result = await api_post("/api/workspace/memory", {"text": value, "project_id": selected})
            if result is None:
                ui.notify("Не удалось сохранить запись", type="negative")
                return
            new_note.set_value("")
            await refresh()

        with ui.row().classes("gap-2"):
            action_button("Запомнить", icon="o_bookmark_add", on_click=remember, variant="primary")
            action_button("Закрыть", on_click=dialog.close, variant="quiet")
    dialog.open()
    await refresh()
