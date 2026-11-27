"""Searchable, offline user guide built from the shared component registry."""
from __future__ import annotations

from nicegui import ui

from sovushka.uikit.components import action_button, panel, section_heading, text_field
from backend.user_guide_content import ARTICLES


def matching_articles(query: str):
    words = query.casefold().split()
    return [article for article in ARTICLES if all(word in " ".join(article[1:]).casefold() for word in words)]


def open_user_guide():
    with ui.dialog() as dialog, panel(classes="sov-light-guide"):
        with ui.row().classes("w-full items-start justify-between flex-nowrap"):
            section_heading("Руководство LES RAG", "Короткие ответы о работе с приложением. Доступно без интернета.")
            action_button(icon="o_close", icon_only=True, aria_label="Закрыть руководство", variant="quiet", on_click=dialog.close)
        search = text_field(label="Найти в руководстве", placeholder="Например: почта, источник, модель", clearable=True, classes="w-full")
        with ui.scroll_area().classes("sov-light-guide-body"):
            results = ui.column().classes("w-full gap-2")

        def render():
            results.clear()
            matches = matching_articles(search.value or "")
            with results:
                if not matches:
                    ui.label("Ничего не найдено. Попробуйте короткое слово: почта, модель или документ.")
                for article_id, category, title, body in matches:
                    with ui.expansion(title, caption=category, value=bool(search.value) or article_id == "start").classes("w-full"):
                        ui.markdown(body).classes("sov-light-guide-text")

        search.on_value_change(lambda _: render())
        render()
    dialog.open()
    return dialog
