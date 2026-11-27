"""User-facing connection recipes; examples never save or bind a model."""
from nicegui import ui

from backend.user_guide_content import CLOUD_GUIDE, LOCAL_GUIDE, TROUBLESHOOTING
from sovushka.uikit import action_button, section_heading

EXAMPLES = {
    "chat": {"display_name": "Ollama — чат", "base_url": "http://127.0.0.1:11434/v1", "model_id": "qwen3.5:2b", "locality": "loopback", "requested_context_tokens": 8192, "extension_type": "ollama"},
    "search": {"display_name": "Ollama — поиск", "base_url": "http://127.0.0.1:11434/v1", "model_id": "bge-m3", "locality": "loopback", "extension_type": "ollama"},
}

def build_connection_help(on_example, *, expanded=False):
    with ui.expansion("Где взять модель и как подключить", icon="o_help_outline", value=expanded).classes("w-full"):
        section_heading("Локально на компьютере — Ollama", "Интернет нужен для загрузки. Модели устанавливаются отдельно от ЛЕС.")
        ui.link("Скачать Ollama для Windows ↗", "https://ollama.com/download/windows", new_tab=True).classes("sov-ui-section-detail")
        ui.markdown(LOCAL_GUIDE).classes("w-full")
        with ui.row().classes("w-full gap-2"):
            action_button("Пример для чата", on_click=lambda: on_example("chat"), variant="secondary")
            action_button("Пример для поиска", on_click=lambda: on_example("search"), variant="secondary")
        ui.label("Примеры открывают заполненную форму. Ничего не скачивается, не сохраняется и не назначается автоматически.").classes("sov-ui-section-detail")
        with ui.row().classes("w-full gap-2"):
            ui.link("Модели Qwen 3.5 ↗", "https://ollama.com/library/qwen3.5", new_tab=True)
            ui.link("Модель поиска BGE-M3 ↗", "https://ollama.com/library/bge-m3", new_tab=True)
        with ui.expansion("Облачный или другой совместимый API").classes("w-full"):
            ui.markdown(CLOUD_GUIDE).classes("w-full")
        with ui.expansion("Если подключение не работает").classes("w-full"):
            ui.markdown(TROUBLESHOOTING).classes("w-full")
