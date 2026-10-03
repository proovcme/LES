"""Reusable progress and bounded user-facing operation journal."""
from __future__ import annotations

import time
from nicegui import ui


class ActivityPanel:
    def __init__(self, label="Подготавливаю запрос"):
        self.started = time.monotonic()
        self.finished = False
        self.last_label = ""
        with ui.column().classes("sov-activity w-full gap-1") as self.root:
            with ui.row().classes("items-center w-full justify-between"):
                self.status = ui.label().props('role="status" aria-live="polite" aria-atomic="true"')
                self.elapsed = ui.label("0 с").props('aria-live="off"')
            self.bar = ui.linear_progress(value=0, show_value=False).props('indeterminate aria-label="Ход операции"')
            with ui.expansion("Ход работы и журнал", icon="o_list_alt").classes("w-full"):
                self.log = ui.log(max_lines=200).classes("w-full h-32").props('aria-label="Журнал этапов" aria-live="off"')
        self.update({"label": label})
        self.timer = ui.timer(1, self.tick)

    def start(self, label):
        self.started = time.monotonic()
        self.finished = False
        self.last_label = ""
        self.root.set_visibility(True)
        self.bar.set_visibility(True)
        self.timer.activate()
        self.update({"label": label})

    def tick(self):
        if not self.finished:
            self.elapsed.set_text(f"{int(time.monotonic() - self.started)} с")

    def update(self, payload):
        if self.finished:
            return
        label = str(payload.get("label") or "Работаю")
        self.tick()
        if label != self.last_label:
            self.last_label = label
            self.status.set_text(label)
            self.log.push(f"{int(time.monotonic() - self.started):>4} с · {label}")
        current, total = payload.get("completed"), payload.get("total")
        if isinstance(current, int) and not isinstance(current, bool) and isinstance(total, int) and not isinstance(total, bool) and total > 0:
            self.bar.props(remove="indeterminate")
            self.bar.set_value(max(0, min(current, total)) / total)
        else:
            self.bar.props("indeterminate")

    def finish(self, label="Готово"):
        if self.finished:
            return
        self.update({"label": label})
        self.finished = True
        self.bar.set_visibility(False)
        self.timer.deactivate()
