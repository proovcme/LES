"""Folder observation controls and persistent per-file activity log."""
from datetime import datetime
from urllib.parse import quote

from nicegui import ui
from sovushka.state import api_get, api_put, last_api_error_text
from sovushka.uikit.components import action_button, panel, section_heading


async def open_dataset_watch(dataset_id, path):
    endpoint = f"/api/rag/datasets/{quote(dataset_id, safe='')}/watch"
    data = await api_get(endpoint)
    if data is None:
        ui.notify(last_api_error_text("Не удалось открыть наблюдение за файлами"), type="negative")
        return
    settings = data["watch"]
    labels = {"disabled": "Выключено", "waiting": "Ожидает проверки", "changes": "Обнаружены изменения",
              "current": "Изменений нет", "indexing": "Передано на индексацию", "synced": "Синхронизировано", "unavailable": "Требуется внимание"}
    dialog = ui.dialog()
    with dialog, panel(variant="raised", classes="w-full max-w-2xl p-4"):
        section_heading("Наблюдение за файлами", "Проверка папки каждые 15 секунд, пока запущен LES RAG.")
        ui.label(path).classes("w-full break-all")
        enabled = ui.switch("Отслеживать изменения", value=bool(settings.get("enabled")))
        auto = ui.switch("Автоматически индексировать изменения", value=bool(settings.get("auto_index")))
        ui.label("Обработка начинается после повторной проверки стабильности файлов. Если папка недоступна, синхронизация приостанавливается.").classes("text-sm")
        status = ui.label(labels.get(settings["status"], "Ожидает проверки"))
        journal = ui.column().classes("w-full max-h-80 overflow-y-auto")

        async def refresh():
            latest = await api_get(endpoint)
            if latest is None:
                status.set_text("Сервис недоступен. Повторите проверку.")
                return
            watch = latest["watch"]
            stamp = watch.get("checked_at")
            checked = datetime.fromtimestamp(stamp).strftime("%d.%m %H:%M:%S") if stamp else "ещё не проверялось"
            status.set_text(f"{labels.get(watch['status'], 'Ожидает проверки')} · {checked}")
            journal.clear()
            with journal:
                if not latest["events"]:
                    ui.label("Изменений пока не зарегистрировано.")
                for event in latest["events"]:
                    when = datetime.fromtimestamp(event["at"]).strftime("%d.%m %H:%M:%S")
                    ui.label(f"{when} · {event['message']} · {event['file_name']}").classes("w-full break-all text-sm")

        async def save():
            result = await api_put(endpoint, {"path": path, "enabled": enabled.value, "auto_index": auto.value})
            if result is None:
                ui.notify(last_api_error_text("Не удалось сохранить наблюдение"), type="negative")
                return
            ui.notify("Настройки наблюдения сохранены", type="positive")
            await refresh()

        with ui.row().classes("w-full justify-end gap-2"):
            action_button("Закрыть", on_click=dialog.close, variant="quiet", compact=True)
            action_button("Обновить журнал", on_click=refresh, variant="quiet", compact=True)
            action_button("Сохранить", on_click=save, variant="primary", compact=True)
    await refresh()
    dialog.open()
