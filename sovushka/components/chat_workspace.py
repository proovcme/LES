"""Project chat navigation and explicit advisory memory, using shared UI controls."""
from __future__ import annotations

from urllib.parse import quote

from nicegui import ui

from sovushka.state import api_delete, api_get, api_patch, api_post
from sovushka.uikit import action_button, checkbox_field, panel, section_heading, select_field, text_field


def _rows(payload, key):
    return payload.get(key, []) if isinstance(payload, dict) else list(payload or [])


def _required(payload):
    if payload is None:
        raise RuntimeError("Не удалось сохранить или загрузить данные. Попробуйте ещё раз.")
    return payload


class ChatWorkspace:
    def __init__(self, *, on_open, get_session_id, is_busy, is_admin=True, on_change=None):
        self.on_open = on_open
        self.get_session_id = get_session_id
        self.is_busy = is_busy
        self.is_admin = is_admin
        self.on_change = on_change
        self.active = {}
        self.project_names = {}
        self.button = None
        self.navigating = False
        self._picker_dialog = None
        self._memory_dialog = None

    def render(self):
        self.button = action_button(
            "Обычный чат", icon="o_folder_open", variant="quiet",
            on_click=lambda: self.run(self.open_picker),
        )
        action_button("Память", icon="o_bookmark_border", variant="quiet",
                      on_click=lambda: self.run(self.open_memory))

    async def run(self, action):
        try:
            return await action()
        except RuntimeError as error:
            ui.notify(str(error), type="negative")
            return None

    async def _navigate(self, action):
        if self.is_busy() or self.navigating:
            raise RuntimeError("Дождитесь завершения ответа перед сменой чата.")
        self.navigating = True
        try:
            return await action()
        finally:
            self.navigating = False

    async def activate(self, record):
        return await self._navigate(lambda: self._commit(record))

    async def _commit(self, record):
        if not await self.on_open(record):
            return False
        self.active = dict(record)
        if self.button:
            pid = record.get("project_id")
            label = self.project_names.get(pid, "Проект") if pid else "Обычный чат"
            self.button.set_text(label)
            self.button.props(f'aria-label="{label.replace(chr(34), chr(39))}"')
        if self.on_change:
            await self.on_change()
        return True

    async def create_project(self, title):
        title = str(title or '').strip()
        if not self.is_admin:
            raise RuntimeError('Создание проекта недоступно для этой учётной записи.')
        if not title:
            raise RuntimeError('Введите название проекта.')
        async def create():
            project = _required(await api_post('/api/projects', {'name': title}))
            self.project_names[project['id']] = project['name']
            return await self._open_project(project['id'])
        return await self._navigate(create)

    async def open_project(self, project_id):
        return await self._navigate(lambda: self._open_project(project_id))

    async def _open_project(self, project_id):
        suffix = f"?project_id={project_id}" if project_id else ""
        sessions = _rows(_required(await api_get(f"/api/workspace/sessions{suffix}")), "sessions")
        if sessions:
            return await self._commit(sessions[0])
        return await self._commit(_required(await api_post("/api/workspace/sessions", {"project_id": project_id})))

    async def open_session(self, session_id):
        async def load():
            return await self._commit(_required(await api_get(f"/api/workspace/sessions/{quote(session_id, safe='')}")))
        return await self._navigate(load)

    async def new_session(self):
        return await self._navigate(self._new_session)

    async def _new_session(self):
        record = _required(await api_post("/api/workspace/sessions", {"project_id": self.active.get("project_id")}))
        return await self._commit(record)

    async def persist(self, *, scope, role, title):
        if self.navigating:
            raise RuntimeError("Дождитесь загрузки чата перед отправкой сообщения.")
        sid = self.get_session_id()
        if self.active.get("session_id") != sid:
            record = await api_get(f"/api/workspace/sessions/{quote(sid, safe='')}")
            self.active = record or _required(await api_post("/api/workspace/sessions", {
                "session_id": sid, "project_id": None, "title": title[:120],
            }))
        self.active = _required(await api_patch(f"/api/workspace/sessions/{quote(sid, safe='')}", {
            "scope": dict(scope), "role": role, "title": self.active.get("title") or title[:120],
        }))
        if self.on_change:
            await self.on_change()
        return self.active

    async def restore(self):
        return await self._navigate(self._restore)

    async def _restore(self):
        sid = self.get_session_id()
        if not sid:
            return
        record = await api_get(f"/api/workspace/sessions/{quote(sid, safe='')}")
        if record and record.get("registered", True):
            projects = _rows(await api_get("/api/projects"), "projects")
            self.project_names = {row["id"]: row["name"] for row in projects}
            await self._commit(record)

    async def open_picker(self):
        if self._picker_dialog and self._picker_dialog.value:
            return
        projects = _rows(_required(await api_get("/api/projects")), "projects")
        self.project_names = {row["id"]: row["name"] for row in projects}
        with ui.dialog() as dialog, panel(variant="raised", classes="sov-workspace-dialog"):
            self._picker_dialog = dialog
            section_heading("Проекты и чаты", "У каждого проекта свои чаты, источники и память.")
            body = ui.column().classes("w-full gap-2")

            async def choose(pid):
                if await self.run(lambda: self.open_project(pid)):
                    dialog.close()

            with body:
                action_button("Обычный чат", icon="o_chat_bubble_outline", on_click=lambda: choose(None), classes="w-full")
                for project in projects:
                    if project.get("status") == "archived":
                        continue
                    action_button(project["name"], icon="o_folder_open",
                                  on_click=lambda pid=project["id"]: choose(pid), classes="w-full")
            if self.is_admin:
                name = text_field(label="Новый проект", placeholder="Название", classes="w-full")

                async def create():
                    title = str(name.value or "").strip()
                    if not title:
                        ui.notify("Введите название проекта", type="warning")
                        return
                    if self.is_busy():
                        ui.notify("Дождитесь завершения ответа", type="warning")
                        return
                    async def save():
                        project = _required(await api_post("/api/projects", {"name": title}))
                        self.project_names[project["id"]] = project["name"]
                        return await self.open_project(project["id"])
                    if await self.run(save):
                        dialog.close()

                action_button("Создать проект", icon="o_add", on_click=create, variant="primary")
            action_button("Закрыть", on_click=dialog.close, variant="quiet")
        dialog.open()

    async def open_memory(self):
        from sovushka.components.workspace_memory_editor import open_workspace_memory
        await open_workspace_memory(self)
