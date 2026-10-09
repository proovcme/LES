"""Keep a failed request discoverable without pulling readers out of history."""
from nicegui import ui


class ChatFailureNotice:
    def __init__(self):
        self.bubble = None
        with ui.row().classes("sov-chat-failure-notice") as self.row:
            ui.label("Запрос не завершён. Вопрос сохранён.").props('role=alert aria-atomic=true')
            ui.button("Показать ошибку", on_click=self.reveal).props('flat no-caps')
        self.row.set_visibility(False)

    def clear(self):
        self.bubble = None
        self.row.set_visibility(False)

    def show(self, bubble):
        self.bubble = bubble
        bubble.props('tabindex=-1 aria-label="Ошибка запроса"')
        self.row.set_visibility(True)

    def reveal(self):
        if self.bubble is None:
            return
        # Only this explicit action moves focus; incoming tokens/errors never do.
        ui.run_javascript(f'''const target = document.getElementById("c{int(self.bubble.id)}");
            if (target) {{ target.scrollIntoView({{block: "nearest", behavior: "instant"}});
                          target.focus({{preventScroll: true}}); }}''')
