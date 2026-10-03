"""No-AI document browser for LES datasets.

This page is an operator surface: dataset -> document -> chunks/search. It
does not ask the model anything; it makes the indexed corpus visible.
"""
from __future__ import annotations


from nicegui import context, ui

from sovushka.uikit.components import action_button, text_field

DATASET_GROUP_OPTIONS = {
    "": "Все",
    "project": "Проекты",
    "other": "Не проекты",
}


from sovushka.components.document_browser import DocumentBrowser
from sovushka.components.document_presentation import _label, _schedule

def build_documents(
    *,
    surface: str = "documents",
    initial_dataset_id: str = "",
    show_dataset_picker: bool = True,
    can_manage: bool = False,
) -> None:
    if surface not in {"documents", "data"}:
        raise ValueError(f"Unknown documents surface: {surface}")
    initial_mode = "map"
    initial_title = {
        "documents": "Выберите файл",
        "data": "Выберите файл",
    }[surface]
    initial_note = {
        "documents": "Выберите файл: покажем только извлечённое содержимое и оригинал.",
        "data": "Файлы, извлечённое содержимое, оригиналы и доказательства.",
    }[surface]
    initial_dataset = str(initial_dataset_id or "").strip()
    if not initial_dataset:
        try:
            initial_dataset = str(context.client.request.query_params.get("dataset_id") or "").strip()
        except (AttributeError, RuntimeError):
            initial_dataset = ""
    state = {
        "datasets": [],
        "documents": [],
        "chunks": [],
        "hits": [],
        "dataset_memory": {},
        "memory_loading": False,
        "operator_guidance": "",
        "dataset_kind": "",
        "pdf_extract": {},
        "pdf_extract_loading": False,
        "view_mode": initial_mode,
        "selected_dataset": initial_dataset,
        "selected_doc_id": "",
        "selected_doc_name": "",
        "selected_doc_ids": [],
        "dataset_filter": "",
        "dataset_kind_filter": "",
        "dataset_group_filter": "",
        "document_filter": "",
        "document_folder_filter": "",
        "document_extension_filter": "",
        "document_status_filter": "",
        "document_role_filter": "",
        "document_tree_open": [],
        "document_map_files": [],
        "document_map_label": "",
        "project_filter": "",
        "composition_view": "tree",
        "selected_folder": "",
        "composition_folder_filter": "",
        "composition_extension_filter": "",
        "composition_status_filter": "",
        "composition_role_filter": "",
        "composition_name_filter": "",
        "composition_file": {},
        "composition_file_loading": False,
        "map_target": "dataset",
        "rag_readiness": {},
        "rag_readiness_loading": False,
        "query": "",
        "view_title": initial_title,
        "view_note": initial_note,
    }
    refs: dict[str, object] = {}
    browser = DocumentBrowser(state, refs, surface=surface, can_manage=can_manage,
                              initial_mode=initial_mode, initial_note=initial_note, initial_dataset=initial_dataset)


    surface_title = {
        "documents": "Документы",
        "data": "Данные",
    }[surface]
    surface_subtitle = {
        "documents": "Содержимое в RAG и оригинал файла",
        "data": "Файлы и доказательства выбранного набора",
    }[surface]

    focused_class = " sov-data-detail--focused" if not show_dataset_picker else ""
    with ui.column().classes(
        f"w-full h-full gap-0 sov-docs-shell sov-ui-shell sov-ui-documents{focused_class}"
    ):
        with ui.row().classes("items-center w-full sov-docs-topbar"):
            with ui.column().classes("sov-docs-heading"):
                _label(surface_title, size="16px", weight=900).classes("sov-docs-title")
                _label(surface_subtitle, size="11.5px", color="var(--dim)").classes(
                    "sov-docs-subtitle"
                )
            if surface == "data":
                def _back_to_data() -> None:
                    request_path = str(getattr(context.client.request, "url", "") or "")
                    target_path = "/les/classic" if "/les/classic" in request_path else "/classic"
                    ui.navigate.to(f"{target_path}?tab=data")

                action_button(
                    "Назад ко всем данным",
                    icon="o_arrow_back",
                    on_click=_back_to_data,
                    variant="quiet",
                    classes="sov-data-detail-back",
                )
            q_input = text_field(
                placeholder="Найти файл, шифр, раздел или текст…",
                clearable=True,
                classes="sov-docs-search",
            )
            with q_input.add_slot("prepend"):
                ui.icon("o_search")
            q_input.on("update:model-value", lambda e: state.__setitem__("query", str(e.args or "")))
            q_input.on("keydown.enter", lambda _e: _schedule(browser._search("dataset" if state["selected_dataset"] else "all")))
            with ui.row().classes("items-center").style("gap:5px;flex-wrap:wrap;") as readiness_summary:
                refs["readiness_summary"] = readiness_summary
            search_button = action_button(
                "Найти",
                icon="o_search",
                on_click=lambda: _schedule(
                    browser._search("dataset" if state["selected_dataset"] else "all")
                ),
                variant="primary",
                aria_label="Искать",
                classes="sov-docs-search-btn",
            )

        with ui.row().classes(f"w-full flex-1 no-wrap sov-docs-workspace{focused_class}"):
            with ui.column().classes("h-full no-wrap sov-docs-datasets-panel") as datasets_column:
                with ui.row().classes("items-center w-full sov-docs-panel-title"):
                    ui.icon("o_dataset")
                    _label(
                        "1. Выберите датасет",
                        size="12px",
                        color="var(--dim)",
                        weight=900,
                    )
                with ui.row().classes("sov-dataset-group-filter"):
                    refs["dataset_group_buttons"] = {}
                    for value, label in DATASET_GROUP_OPTIONS.items():
                        active = str(state.get("dataset_group_filter") or "") == value
                        group_button = ui.button(
                            label,
                            on_click=lambda _e, group=value: browser._set_dataset_group_filter(group),
                        ).props("flat no-caps").classes(
                            "sov-dataset-group-btn sov-dataset-group-btn--active" if active else "sov-dataset-group-btn"
                        )
                        refs["dataset_group_buttons"][value] = group_button
                dataset_filter = ui.input(placeholder="Название датасета…").props("outlined clearable").classes("sov-docs-filter")
                dataset_filter.on(
                    "update:model-value",
                    lambda e: (state.__setitem__("dataset_filter", str(e.args or "")), browser.view._render_datasets()),
                )
                dataset_filter.on("keydown.enter", lambda _e: _schedule(browser._load_datasets(select_first=True)))
                with ui.column().classes("w-full gap-2 sov-docs-list") as datasets_panel:
                    refs["datasets"] = datasets_panel
                datasets_column.set_visibility(show_dataset_picker)

            with ui.column().classes("h-full no-wrap sov-docs-files-panel") as files_column:
                with ui.row().classes("items-center w-full sov-docs-panel-title"):
                    ui.icon("o_folder_copy")
                    _label(
                        "2. Выберите файлы",
                        size="12px",
                        color="var(--dim)",
                        weight=900,
                    )
                refs["dataset_data_button"] = ui.button(
                    "Данные о датасете",
                    icon="o_dataset",
                    on_click=browser._show_dataset_data,
                ).props("flat no-caps").classes("w-full sov-dataset-data-button")
                refs["dataset_data_button"].set_visibility(False)
                service_upload = ui.upload(
                    label="Добавить файл",
                    auto_upload=True,
                    max_files=1,
                    on_upload=browser._upload_service_file,
                ).props("flat accept=.xlsx,.xlsm,.xls,.csv,.pdf,.docx,.md,.txt,.json,.yaml").classes(
                    "w-full sov-service-file-upload"
                )
                service_upload.set_visibility(False)
                refs["service_upload"] = service_upload
                document_filter = ui.input(placeholder="Название файла…").props("outlined clearable").classes("sov-docs-filter")
                refs["document_filter"] = document_filter
                document_filter.on(
                    "update:model-value",
                    lambda e: browser._set_document_text_filter(str(e.args or "")),
                )
                document_filter.on("keydown.enter", lambda _e: _schedule(browser._load_documents()))
                with ui.column().classes("w-full gap-2 sov-docs-list") as documents_panel:
                    refs["documents"] = documents_panel

            with ui.column().classes("h-full no-wrap sov-docs-view-panel") as view_panel:
                refs["view"] = view_panel

    ui.timer(0.15, lambda: _schedule(browser._load_surface()), once=True)
