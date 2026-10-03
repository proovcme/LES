"""Dataset list, file tree and source reader presentation."""
from __future__ import annotations

from urllib.parse import quote
from nicegui import ui
from sovushka.state import api_patch, add_log, last_api_error_text
from sovushka.components.document_presentation import _badge, _dataset_group, _dataset_title, _file_icon, _file_kind, _format_size, _label, _plain_index_text, _readiness_label, _schedule, _short_path
from typing import TYPE_CHECKING
if TYPE_CHECKING:
    from sovushka.components.document_browser import DocumentBrowser


class DocumentViews:
    def __init__(self, browser: DocumentBrowser):
        self.browser = browser

    def _render_status_error(self) -> None:
        err = last_api_error_text() or "proxy не вернул данные документов"
        add_log(f"[DOCS] {err}")
        ui.notify(err, type="negative")

    def _render_readiness_summary(self) -> None:
        panel = self.browser.refs.get("readiness_summary")
        if panel is None:
            return
        panel.clear()
        with panel:
            if self.browser.state.get("rag_readiness_loading"):
                ui.spinner(size="sm")
                return
            readiness = self.browser.state.get("rag_readiness") if isinstance(self.browser.state.get("rag_readiness"), dict) else {}
            general = readiness.get("general") if isinstance(readiness.get("general"), dict) else {}
            general_label, general_cls = _readiness_label(general)
            _badge(f"RAG: {general_label}", general_cls)

    async def _prompt_rename_dataset(self, dataset_id: str, current_name: str) -> None:
        with ui.dialog() as dlg, ui.card().classes("p-4 w-96"):
            ui.label("Переименование датасета").classes("text-base font-bold mb-2")
            inp = ui.input("Название датасета", value=current_name).classes("w-full mb-4")
            with ui.row().classes("justify-end w-full gap-2"):
                ui.button("Отмена", on_click=dlg.close).props("flat")
                async def _save():
                    new_val = inp.value.strip()
                    if not new_val:
                        ui.notify("Название не может быть пустым", type="warning")
                        return
                    res = await api_patch(f"/api/rag/datasets/{quote(dataset_id, safe='')}/name", {"name": new_val})
                    if res:
                        ui.notify("Датасет переименован", type="positive")
                        dlg.close()
                        await self.browser._load_datasets(select_first=False)
                    else:
                        ui.notify(last_api_error_text("Ошибка переименования"), type="negative")
                ui.button("Сохранить", on_click=_save).props("primary")
        dlg.open()

    async def _prompt_set_group_dataset(self, dataset_id: str, current_group: str) -> None:
        with ui.dialog() as dlg, ui.card().classes("p-4 w-96"):
            ui.label("Изменить группу датасета").classes("text-base font-bold mb-2")
            inp = ui.input("Имя группы (например, Проекты, Почта)", value=current_group).classes("w-full mb-4")
            with ui.row().classes("justify-end w-full gap-2"):
                ui.button("Отмена", on_click=dlg.close).props("flat")
                async def _save():
                    new_val = inp.value.strip()
                    res = await api_patch(f"/api/rag/datasets/{quote(dataset_id, safe='')}/group", {"group": new_val})
                    if res:
                        ui.notify("Группа обновлена", type="positive")
                        dlg.close()
                        await self.browser._load_datasets(select_first=False)
                    else:
                        ui.notify(last_api_error_text("Ошибка изменения группы"), type="negative")
                ui.button("Сохранить", on_click=_save).props("primary")
        dlg.open()

    def _render_datasets(self) -> None:
        panel = self.browser.refs.get("datasets")
        if panel is None:
            return
        panel.clear()
        with panel:
            if not self.browser.state["datasets"]:
                _label("Датасетов не найдено", color="var(--dim)")
                return
            kind_filter = str(self.browser.state.get("dataset_kind_filter") or "")
            group_filter = str(self.browser.state.get("dataset_group_filter") or "")
            rows = [
                row for row in self.browser.state["datasets"]
                if (not kind_filter or str(row.get("dataset_kind") or "") == kind_filter)
                and (not group_filter or _dataset_group(row) == group_filter)
            ]
            if not rows:
                _label("Датасетов с такой меткой нет", color="var(--dim)")
                return
            previous_scope = ""
            for row in rows:
                current_scope = "system" if self.browser._is_system_dataset(row) else "user"
                if current_scope != previous_scope:
                    _label(
                        "Служебные" if current_scope == "system" else "Пользовательские",
                        size="10px",
                        color="var(--dim)",
                        weight=900,
                    ).style("margin:8px 2px 2px;text-transform:uppercase;letter-spacing:.05em;")
                    previous_scope = current_scope
                did = str(row.get("id") or "")
                selected = did == self.browser.state["selected_dataset"]
                selected_cls = " sov-dataset-card--selected" if selected else ""
                with ui.element("div").classes(f"w-full sov-dataset-card{selected_cls}").on(
                    "click", lambda _e, value=did: _schedule(self.browser._select_dataset(value))
                ):
                    with ui.row().classes("items-center w-full sov-dataset-card-head"):
                        with ui.element("div").classes("sov-dataset-icon"):
                            ui.icon("o_push_pin" if self.browser._is_system_dataset(row) else "o_folder_open")
                        _label(_dataset_title(row), size="13px", weight=850).classes("sov-dataset-name")
                        if self.browser.can_manage and not self.browser._is_system_dataset(row):
                            d_name = _dataset_title(row)
                            d_grp = str(row.get("group_name") or "")
                            with ui.row().classes("items-center gap-0.5 ml-auto"):
                                ui.button(
                                    icon="o_edit",
                                    on_click=lambda _e, id=did, nm=d_name: _schedule(self._prompt_rename_dataset(id, nm)),
                                ).props('flat dense round aria-label="Переименовать"').tooltip("Переименовать датасет")
                                ui.button(
                                    icon="o_label",
                                    on_click=lambda _e, id=did, gr=d_grp: _schedule(self._prompt_set_group_dataset(id, gr)),
                                ).props('flat dense round aria-label="Группа"').tooltip("Изменить группу датасета")
                        ui.icon("o_chevron_right").classes("sov-dataset-chevron")
                    status_ready = str(row.get("status", "")).upper() in {"IDLE", "INDEXED"}
                    meta = [
                        "служебный датасет" if self.browser._is_system_dataset(row) else (
                            "проект" if _dataset_group(row) == "project" else "база знаний"
                        ),
                        f"{int(row.get('document_count') or 0)} файлов",
                        "готов" if status_ready else "индексируется",
                    ]
                    if selected:
                        readiness = self.browser.state.get("rag_readiness") if isinstance(self.browser.state.get("rag_readiness"), dict) else {}
                        general = readiness.get("general") if isinstance(readiness.get("general"), dict) else {}
                        meta.append("RRF готов" if general.get("rrf_ready") else "RRF не готов")
                    _label("  ·  ".join(meta), size="10.5px", color="var(--dim)").classes("sov-dataset-meta-text")
                    pending = int(row.get("pending_count") or 0)
                    errors = int(row.get("error_count") or 0)
                    missing = int(row.get("missing_count") or 0)
                    attention = []
                    if pending:
                        attention.append(f"{pending} ожидает")
                    if errors:
                        attention.append(f"{errors} ошибок")
                    if missing:
                        attention.append(f"{missing} отсутствует")
                    if attention:
                        _label(" · ".join(attention), size="10.3px", color="var(--warn)").classes("sov-dataset-attention")

    def _render_documents(self) -> None:
        panel = self.browser.refs.get("documents")
        if panel is None:
            return
        panel.clear()
        with panel:
            if not self.browser.state["selected_dataset"]:
                _label("Сначала выберите датасет", color="var(--dim)")
                return
            if not self.browser.state["documents"]:
                _label("Документы не найдены", color="var(--dim)")
                return
            selected_ids = {str(value) for value in (self.browser.state.get("selected_doc_ids") or [])}
            all_documents = [row for row in self.browser.state["documents"] if isinstance(row, dict)]
            folder_options = sorted(
                {
                    str(row.get("file_name") or "").rsplit("/", 1)[0]
                    for row in all_documents
                    if "/" in str(row.get("file_name") or "")
                },
                key=str.casefold,
            )
            extension_options = sorted({_file_kind(str(row.get("file_name") or "")) for row in all_documents})
            status_options = sorted(
                {str(row.get("status") or "").upper() for row in all_documents if row.get("status")}
            )
            role_options = sorted(
                {
                    str(row.get("doc_type") or row.get("content_type") or row.get("domain") or "").strip()
                    for row in all_documents
                    if str(row.get("doc_type") or row.get("content_type") or row.get("domain") or "").strip()
                },
                key=str.casefold,
            )
            folder_filter = str(self.browser.state.get("document_folder_filter") or "")
            extension_filter = str(self.browser.state.get("document_extension_filter") or "")
            status_filter = str(self.browser.state.get("document_status_filter") or "")
            role_filter = str(self.browser.state.get("document_role_filter") or "")
            with ui.row().classes("w-full sov-file-panel-filters"):
                folder_select = ui.select(
                    {"": "Все папки", **{value: value for value in folder_options}},
                    value=folder_filter,
                    label="Папка",
                ).props("outlined dense options-dense")
                folder_select.on(
                    "update:model-value",
                    lambda e: self.browser._set_document_file_filter("document_folder_filter", str(e.args or "")),
                )
                extension_select = ui.select(
                    {"": "Все форматы", **{value: value for value in extension_options}},
                    value=extension_filter,
                    label="Формат",
                ).props("outlined dense options-dense")
                extension_select.on(
                    "update:model-value",
                    lambda e: self.browser._set_document_file_filter("document_extension_filter", str(e.args or "")),
                )
                status_select = ui.select(
                    {"": "Все статусы", **{value: value for value in status_options}},
                    value=status_filter,
                    label="Статус",
                ).props("outlined dense options-dense")
                status_select.on(
                    "update:model-value",
                    lambda e: self.browser._set_document_file_filter("document_status_filter", str(e.args or "")),
                )
                status_select.set_visibility(False)
                role_select = ui.select(
                    {"": "Все типы", **{value: value for value in role_options}},
                    value=role_filter,
                    label="Тип",
                ).props("outlined dense options-dense")
                role_select.on(
                    "update:model-value",
                    lambda e: self.browser._set_document_file_filter("document_role_filter", str(e.args or "")),
                )
                role_select.set_visibility(False)
            if self.browser.surface == "documents":
                with ui.row().classes("items-center w-full").style("gap:6px;padding:2px 4px 6px;"):
                    _label(
                        f"Выбрано для вопроса: {len(selected_ids)}",
                        size="10.5px",
                        color="var(--dim)",
                        weight=800,
                    ).style("flex:1;")
                    if selected_ids:
                        ui.button(
                            icon="o_close",
                            on_click=lambda: (
                                self.browser.state.__setitem__("selected_doc_ids", []),
                                self._render_documents(),
                                self._render_view(),
                            ),
                        ).props('flat round dense aria-label="Снять выбор"')
            dataset_data_button = self.browser.refs.get("dataset_data_button")
            if dataset_data_button is not None:
                dataset_data_button.classes(remove="sov-dataset-data-button--active")
                if self.browser.state.get("view_mode") == "map" and self.browser.state.get("map_target") == "dataset":
                    dataset_data_button.classes(add="sov-dataset-data-button--active")
            needle = str(self.browser.state.get("document_filter") or "").strip().casefold()
            map_files = {str(value) for value in (self.browser.state.get("document_map_files") or []) if str(value)}
            rows = [
                row for row in self.browser.state["documents"]
                if (
                    (map_files and str(row.get("file_name") or "") in map_files)
                    or (not map_files and (not needle or needle in str(row.get("file_name") or "").casefold()))
                )
                and (
                    not folder_filter
                    or str(row.get("file_name") or "").startswith(folder_filter.rstrip("/") + "/")
                )
                and (not extension_filter or _file_kind(str(row.get("file_name") or "")) == extension_filter)
                and (not status_filter or str(row.get("status") or "").upper() == status_filter)
                and (
                    not role_filter
                    or str(row.get("doc_type") or row.get("content_type") or row.get("domain") or "").strip() == role_filter
                )
            ]
            if self.browser.state.get("document_map_label"):
                with ui.row().classes("items-center w-full sov-document-map-filter"):
                    ui.icon("o_filter_alt")
                    _label(
                        f"Раздел {self.browser.state.get('document_map_label')} · {len(rows)} файлов",
                        size="10.8px",
                        weight=800,
                    ).style("flex:1;")
                    ui.button(
                        icon="o_close",
                        on_click=lambda: self.browser._filter_documents_from_map(""),
                    ).props('flat round dense aria-label="Сбросить фильтр раздела"')
            selected_dataset_name = _dataset_title(self.browser._selected_dataset_row()).casefold()
            folders: dict[str, dict] = {"": {"name": "", "path": "", "parent": "", "files": []}}
            direct_files: dict[str, list[dict]] = {"": []}
            for row in rows:
                file_name = str(row.get("file_name") or "")
                parts = [part for part in file_name.split("/") if part]
                if parts and parts[0].casefold() == selected_dataset_name:
                    parts = parts[1:]
                directory_parts = parts[:-1]
                directory = "/".join(directory_parts)
                direct_files.setdefault(directory, []).append(row)
                folders[""]["files"].append(row)
                for depth in range(1, len(directory_parts) + 1):
                    path = "/".join(directory_parts[:depth])
                    parent = "/".join(directory_parts[: depth - 1])
                    folder = folders.setdefault(
                        path,
                        {"name": directory_parts[depth - 1], "path": path, "parent": parent, "files": []},
                    )
                    folder["files"].append(row)
            children: dict[str, list[dict]] = {}
            for path, folder in folders.items():
                if path:
                    children.setdefault(str(folder["parent"]), []).append(folder)
            for items in children.values():
                items.sort(key=lambda item: str(item.get("name") or "").casefold())

            def _render_document_row(row: dict) -> None:
                doc_id = str(row.get("id") or "")
                selected = doc_id in {
                    str(self.browser.state.get("selected_doc_id") or ""),
                    str((self.browser.state.get("composition_file") or {}).get("doc_id") or ""),
                } or doc_id in selected_ids
                file_name = str(row.get("file_name") or doc_id)
                basename = file_name.rsplit("/", 1)[-1]
                folder = file_name.rsplit("/", 1)[0] if "/" in file_name else ""
                selected_cls = " sov-document-card--selected" if selected else ""
                with ui.element("div").classes(f"w-full sov-document-card sov-document-card--tree{selected_cls}").on(
                    "click", lambda _e, value=doc_id, name=file_name: self.browser._activate_document_row(value, name)
                ):
                    with ui.row().classes("items-center w-full sov-document-card-head"):
                        if self.browser.surface == "documents":
                            select_btn = ui.button(
                                icon="o_check_box" if doc_id in selected_ids else "o_check_box_outline_blank",
                            ).props('flat round dense aria-label="Выбрать документ"').classes("sov-icon-btn")
                            select_btn.on("click.stop", lambda _e, value=doc_id: self.browser._toggle_document_selection(value))
                        with ui.element("div").classes("sov-document-icon"):
                            ui.icon(_file_icon(file_name))
                        with ui.column().classes("sov-document-copy"):
                            _label(basename, size="12.5px", weight=850).classes("sov-document-name")
                            if folder:
                                _label(_short_path(folder, parts=3), size="10.5px", color="var(--dim)").classes(
                                    "sov-document-path"
                                )
                    indexed = str(row.get("status", "")).upper() == "INDEXED"
                    meta = [
                        _file_kind(file_name),
                        _format_size(row.get("file_size")),
                        "есть в RAG" if indexed else "нет текста в RAG",
                    ]
                    _label("  ·  ".join(meta), size="10.4px", color="var(--dim)").classes("sov-document-meta-text")

            open_paths = {str(item) for item in (self.browser.state.get("document_tree_open") or []) if str(item)}

            def _render_folder(parent: str = "", depth: int = 0) -> None:
                for folder in children.get(parent, []):
                    path = str(folder.get("path") or "")
                    count = len(folder.get("files") or [])
                    folder_expansion = ui.expansion(
                        f"{folder.get('name') or 'Папка'} · {count}",
                        icon="o_folder",
                        value=bool(needle) or path in open_paths,
                    ).classes("w-full sov-doc-tree-folder").props("dense")
                    folder_expansion.on_value_change(
                        lambda event, value=path: self.browser._remember_document_tree_folder(value, bool(event.value))
                    )
                    with folder_expansion:
                        for row in direct_files.get(path, []):
                            _render_document_row(row)
                        if depth < 7:
                            _render_folder(path, depth + 1)

            _render_folder()
            for row in direct_files.get("", []):
                _render_document_row(row)
            if not rows:
                _label("Файлы и папки не найдены", color="var(--dim)")
            if self.browser.surface in {"documents", "data"} and selected_ids:
                with ui.element("div").classes("sov-docs-sticky-ask"):
                    with ui.column().classes("gap-0").style("min-width:0;flex:1;"):
                        _label(
                            f"{len(selected_ids)} файл(ов) выбрано",
                            size="12px",
                            weight=900,
                        )
                        _label(
                            "В чате область и список файлов будут закреплены.",
                            size="10.5px",
                            color="var(--dim)",
                        )
                    ui.button(
                        "Спросить в чате",
                        icon="o_forum",
                        on_click=self.browser._ask_about_selected_documents,
                    ).props("unelevated no-caps").classes("sov-docs-sticky-ask-button")

    def _render_document_reader(self) -> None:
        """One-purpose document view: indexed content plus the original file."""
        file_data = self.browser.state.get("composition_file") if isinstance(self.browser.state.get("composition_file"), dict) else {}
        selected_doc_id = str(file_data.get("doc_id") or self.browser.state.get("selected_doc_id") or "")
        selected_name = str(file_data.get("file_name") or self.browser.state.get("selected_doc_name") or "")
        if self.browser.state.get("composition_file_loading"):
            with ui.row().classes("items-center sov-document-reader-loading"):
                ui.spinner(size="sm")
                _label("Читаю содержимое из RAG…", size="12px", color="var(--dim)")
            return
        if self.browser.state.get("hits"):
            rows = [item for item in self.browser.state.get("hits") or [] if isinstance(item, dict)]
            with ui.element("section").classes("sov-document-reader-summary"):
                _label(
                    f"Результаты поиска · {len(rows)}",
                    size="13px",
                    weight=900,
                ).classes("sov-document-reader-heading")
                _label(
                    "Показаны фрагменты, которые реально есть в индексе.",
                    size="11px",
                    color="var(--dim)",
                ).classes("sov-document-reader-note")
        elif selected_doc_id:
            rows = [item for item in file_data.get("chunks") or self.browser.state.get("chunks") or [] if isinstance(item, dict)]
            with ui.element("section").classes("sov-document-reader-summary"):
                with ui.row().classes("items-center w-full sov-document-reader-file"):
                    with ui.element("div").classes("sov-document-reader-icon"):
                        ui.icon(_file_icon(selected_name))
                    with ui.column().classes("gap-0").style("min-width:0;flex:1;"):
                        _label(
                            selected_name.rsplit("/", 1)[-1] or "Документ",
                            size="15px",
                            weight=900,
                        ).classes("sov-document-reader-heading")
                        _label(
                            f"В RAG: {int(file_data.get('total') or len(rows))} фрагментов",
                            size="11px",
                            color="var(--dim)",
                        ).classes("sov-document-reader-note")
                    ui.button(
                        "Показать оригинал",
                        icon="o_open_in_new",
                        on_click=lambda _e, name=selected_name, value=selected_doc_id: _schedule(
                            self.browser._open_native_file_name(name, value)
                        ),
                    ).props("unelevated no-caps").classes(
                        "sov-document-reader-original"
                    )
        elif self.browser.state.get("selected_dataset"):
            indexed = sum(
                1
                for item in self.browser.state.get("documents") or []
                if str(item.get("status") or "").upper() == "INDEXED"
            )
            with ui.element("section").classes("sov-document-reader-empty"):
                ui.icon("o_description")
                _label("Выберите файл", size="14px", weight=900)
                _label(
                    f"В этом датасете доступно файлов: {indexed}.",
                    size="11.5px",
                    color="var(--dim)",
                )
            return
        else:
            with ui.element("section").classes("sov-document-reader-empty"):
                ui.icon("o_folder_open")
                _label("Выберите датасет и файл", size="14px", weight=900)
            return

        if not rows:
            with ui.element("section").classes("sov-document-reader-empty"):
                _label("Извлечённого текста для этого файла нет.", size="12px", color="var(--dim)")
            return
        _label("Что есть в RAG", size="13px", weight=900).classes(
            "sov-document-reader-section-title"
        )
        for index, item in enumerate(rows[:40], 1):
            doc_name = str(item.get("doc_name") or selected_name or "")
            heading = _plain_index_text(
                item.get("section_heading") or item.get("parent_heading")
            )
            text = str(item.get("snippet") or item.get("text") or "").strip()
            page = item.get("page") or item.get("source_page") or item.get("page_number")
            with ui.element("article").classes("sov-document-reader-fragment"):
                with ui.row().classes("items-center w-full sov-document-reader-fragment-head"):
                    _label(
                        heading or (f"Страница {page}" if page else f"Фрагмент {index}"),
                        size="12px",
                        weight=850,
                    ).classes("sov-document-reader-fragment-title")
                    if doc_name and (self.browser.state.get("hits") or doc_name != selected_name):
                        _label(
                            doc_name.rsplit("/", 1)[-1],
                            size="10.5px",
                            color="var(--dim)",
                        ).classes("sov-document-reader-source")
                    result_doc_id = str(item.get("doc_id") or "")
                    if self.browser.state.get("hits") and result_doc_id:
                        ui.button(
                            icon="o_open_in_new",
                            on_click=lambda _e, value=result_doc_id: _schedule(
                                self.browser._open_native_document(value)
                            ),
                        ).props(
                            'flat round aria-label="Показать оригинал"'
                        ).classes("sov-document-reader-result-open").tooltip(
                            "Показать оригинал"
                        )
                _label(text, size="11.5px").classes("sov-document-reader-fragment-text")

    def _render_view(self) -> None:
        panel = self.browser.refs.get("view")
        if panel is None:
            return
        panel.clear()
        with panel:
            with ui.row().classes("items-center w-full sov-docs-view-head"):
                with ui.element("div").classes("sov-docs-view-icon"):
                    ui.icon("o_folder_open")
                _label(self.browser.state["view_title"], size="15px", weight=900).classes("sov-docs-view-title").style(
                    "overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:1;"
                )
            _label(self.browser.state["view_note"], size="11.5px", color="var(--dim)").classes("sov-docs-view-note")
            self._render_document_reader()

    def _render_all(self) -> None:
        self._render_datasets()
        self._render_documents()
        self._render_view()
