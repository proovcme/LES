"""Artifact panel lifecycle, previews and downloads for one chat client."""
from __future__ import annotations
from sovushka.components.chat_rendering import (
    _answer_model_label,
    _artifact_from_meta,
    _artifact_present,
    _bubble_text,
    _copy_button,
    _copy_js,
    _download_file_artifact,
    _inventory_file_rows_from_meta,
    _kind_from_name,
    _model_label,
    _parse_json_from_ai,
    _parse_markdown_table,
    _parse_mermaid_from_ai,
    _parse_svg_from_ai,
    _parse_table_from_ai,
    _render_ai_placeholder,
    _render_answer_actions,
    _render_answer_timing,
    _render_dataset_scope_badge,
    _render_empty_artifacts,
    _render_evidence_header,
    _render_excerpts,
    _render_gost_spec,
    _render_model_badge,
    _render_table,
    _render_table_query,
    _render_tree,
    _rows_from_spreadsheet,
    _rows_to_csv,
    _source_label,
)
from sovushka.components.chat_presentation import format_chat_duration_sec, workbook_chat_filename, artifact_workbook_files, format_chat_request_clock, resolve_answer_timing, format_answer_timing_line, _operator_status_chips, _operator_technical_chips, _dataset_profile_operator_summary, _dataset_notebook_operator_summary, _chat_profile_operator_summary, _attachment_chat_payload, _preserved_attachment, _attachment_visible_text, _attachment_user_suffix, _runtime_guard_reason_label
import asyncio
import inspect
import json
import re
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Optional
from nicegui import context, ui
from backend.product_edition import is_light
from sovushka.components.charts import _html, esc
from sovushka.components.chat_workspace import ChatWorkspace
from sovushka.components.chat_project_navigation import ChatProjectNavigation
from sovushka.safe_markup import sanitize_svg
from sovushka.state import (
    add_log,
    api_delete,
    api_get,
    api_get_bytes,
    api_patch,
    api_post,
    api_post_file,
    api_post_stream,
    should_retry_unstreamed_chat,
    last_api_error_text,
    refresh_indexing_mode,
    refresh_samovar,
    state,
)
from sovushka.uikit import action_button, checkbox_field, panel, section_heading, select_field, text_field

OUTPUT_FORMATS = {
    "text": ("Текст", "Свободный ответ"),
    "rag": ("РАГ", "Заземлённый ответ из документов (с цитатами)"),
    "agent": ("Агент", "Исследование в источниках, интернете и разрешённых папках"),
    "kp": ("КП", "Коммерческое предложение (задел на будущее)"),
    "review": ("Проверка проекта", "Нормоконтроль документов объекта"),
    "free": ("Свободный", "Вольный ответ модели без источников (возможны неточности)"),
    "spec": ("Спецификация", "JSON-таблица изделий"),
    "schema": ("Схема", "Иерархия или дерево"),
    "structure": ("Структура", "JSON-объект"),
    "table": ("Таблица", "JSON-массив строк"),
    "mermaid": ("Диаграмма", "Mermaid"),
    "svg": ("SVG", "Векторная схема"),
    "template": ("По образцу", "Структура файла"),
    "verify": ("Верификация", "Сверка скана с распознанным"),
}

class ChatArtifacts:
    def __init__(self, *, _ask_about_inventory_file, _ask_about_inventory_status, _restudy_inventory_dataset, artifact_divider, artifact_panel, artifact_shell, chat_shell, files_artifacts_panel, tab_mermaid, tabs):
        self._ask_about_inventory_file = _ask_about_inventory_file
        self._ask_about_inventory_status = _ask_about_inventory_status
        self._restudy_inventory_dataset = _restudy_inventory_dataset
        self.artifact_divider = artifact_divider
        self.artifact_panel = artifact_panel
        self.artifact_shell = artifact_shell
        self.chat_shell = chat_shell
        self.files_artifacts_panel = files_artifacts_panel
        self.tab_mermaid = tab_mermaid
        self.tabs = tabs
        self._file_artifacts: dict[str, dict] = {}

    def _set_artifacts_visible(self, visible: bool) -> None:
        self.chat_shell.classes(remove="sov-forest-auto-sources")
        self.artifact_shell.set_visibility(visible)
        self.artifact_divider.set_visibility(visible)
        if visible:
            self.chat_shell.classes(remove="sov-artifacts-collapsed")
        else:
            self.chat_shell.classes(add="sov-artifacts-collapsed")


    def _open_artifacts(self) -> None:
        self._set_artifacts_visible(True)


    def _render_project_inventory_artifact(self, meta: dict | None) -> bool:
        rows = _inventory_file_rows_from_meta(meta)
        if not rows:
            return False
        artifact = (meta or {}).get("artifact") if isinstance((meta or {}).get("artifact"), dict) else {}
        title = str(artifact.get("title") or "Реестр файлов")
        indexed = sum(1 for row in rows if row.get("status") == "INDEXED")
        pending = sum(1 for row in rows if row.get("status") == "PENDING")
        errors = sum(1 for row in rows if row.get("status") == "ERROR")
        self._open_artifacts()
        self.artifact_panel.clear()
        with self.artifact_panel:
            with ui.card().classes("sov-artifact-card"):
                with ui.row().classes("w-full items-center justify-between gap-2"):
                    _html(f'<div class="sov-panel-title">{esc(title)}</div>')
                    _copy_button(
                        "JSON",
                        json.dumps(rows, ensure_ascii=False, indent=2),
                        props="no-caps flat dense",
                    )
                _html(
                    '<div class="sov-muted">'
                    f'Файлов: {len(rows)} · INDEXED {indexed} · PENDING {pending} · ERROR {errors}'
                    '</div>'
                )
                with ui.row().classes("w-full items-center gap-2"):
                    ui.button(
                        "Переизучить",
                        icon="o_travel_explore",
                        on_click=lambda: asyncio.create_task(self._restudy_inventory_dataset()),
                    ).props("no-caps flat dense").classes("sov-inventory-ask-btn").tooltip(
                        "Попросить модель заново построить карту датасета"
                    )
                    for st, count in (("INDEXED", indexed), ("PENDING", pending), ("ERROR", errors)):
                        if count:
                            ui.button(
                                f"{st} · {count}",
                                on_click=lambda s=st: asyncio.create_task(self._ask_about_inventory_status(s)),
                            ).props("no-caps flat dense").classes("sov-inventory-ask-btn").tooltip(
                                f"Спросить про файлы со статусом {st}"
                            )
                with ui.column().classes("sov-inventory-files"):
                    for row in rows:
                        file_name = str(row.get("file_name") or "")
                        status = str(row.get("status") or "UNKNOWN")
                        status_cls = {
                            "INDEXED": "sov-inventory-status-indexed",
                            "PENDING": "sov-inventory-status-pending",
                            "ERROR": "sov-inventory-status-error",
                        }.get(status, "sov-inventory-status-unknown")
                        chunks = row.get("chunk_count")
                        with ui.element("div").classes("sov-inventory-file-row"):
                            with ui.column().classes("sov-inventory-file-main"):
                                ui.label(str(row.get("display_name") or file_name)).classes("sov-inventory-file-name")
                                if row.get("folder"):
                                    ui.label(str(row["folder"])).classes("sov-inventory-file-folder")
                                if row.get("document_role"):
                                    ui.label(str(row["document_role"])).classes("sov-inventory-file-role")
                            with ui.row().classes("sov-inventory-file-meta"):
                                labels = row.get("content_layer_labels") or row.get("content_layers") or []
                                for layer_label in labels[:5]:
                                    ui.label(str(layer_label)).classes("sov-inventory-layer")
                                ui.label(status).classes(f"sov-inventory-status {status_cls}")
                                if chunks is not None:
                                    ui.label(f"{chunks} чанков").classes("sov-inventory-chunks")
                                ui.button(
                                    "Спросить по файлу",
                                    icon="o_chat",
                                    on_click=lambda f=file_name: asyncio.create_task(self._ask_about_inventory_file(f)),
                                ).props("no-caps flat dense").classes("sov-inventory-ask-btn").tooltip(
                                    f"Отправить чат с target_file={file_name}"
                                )
        return True


    def _show_artifact(self, ans: str, mode: str) -> None:
        """Открыть артефакт сообщения в панели «Артефакты» (как карточка в Claude Desktop)."""
        self._open_artifacts()
        artifact_mode = mode or "text"
        self._render_result(
            ans,
            artifact_mode if (artifact_mode in OUTPUT_FORMATS or artifact_mode == "markdown") else "text",
            self.artifact_panel,
        )
        try:
            ui.run_javascript(
                "document.querySelector('.sov-artifacts-panel')?.scrollIntoView({behavior:'smooth',block:'start'})"
            )
        except Exception:
            pass


    def _show_meta_artifact(self, meta: dict | None, ans: str, mode: str, srcs: list | None = None) -> None:
        if self._render_project_inventory_artifact(meta):
            return
        meta_artifact = _artifact_from_meta(meta)
        if not meta_artifact:
            self._show_artifact(ans, mode)
            return
        self._show_artifact(
            str(meta_artifact.get("content") or ans or ""),
            str(meta_artifact.get("mode") or mode or "text"),
        )


    def _artifact_button(self, ans: str, mode: str, meta: dict | None = None, srcs: list | None = None) -> None:
        """Кнопка-карточка артефакта в пузыре ответа (если артефакт есть)."""
        meta_artifact = _artifact_from_meta(meta)
        content = str(meta_artifact.get("content") or ans or "")
        artifact_mode = str(meta_artifact.get("mode") or mode or "text")
        if not meta_artifact and not _artifact_present(ans, mode):
            return
        # В model-first сметах таблица внутри Markdown — часть человеческого ответа,
        # а не отдельный "артефакт". Иначе в пузыре появляется шумная кнопка
        # "Артефакт: Таблица" для обычной ВОР.
        if not meta_artifact and str(mode or "text") == "text":
            return
        has_inventory = bool(_inventory_file_rows_from_meta(meta))
        lbl = (
            "Реестр файлов"
            if has_inventory
            else str(
                meta_artifact.get("title")
                or (OUTPUT_FORMATS[mode][0] if (mode in OUTPUT_FORMATS and mode != "text") else "Таблица")
            )
        )
        ui.button(
            f"Артефакт: {lbl} — открыть",
            icon="o_table_view",
            on_click=lambda a=content, m=artifact_mode, md=meta, ss=list(srcs or []): self._show_meta_artifact(md, a, m, ss),
        ).props("no-caps flat dense").classes("sov-artifact-chip").style(
            "margin-top:6px;border:1px solid var(--border);border-radius:8px;"
            "background:var(--bg);color:var(--accent);font-size:.68rem;font-weight:700;padding:4px 10px;"
        )


    async def _preview_file_artifact(self, url: str, name: str, kind: str) -> None:
        """Открыть файл-артефакт в живой панели: xlsx/csv → таблица, картинка → image,
        прочее → скачивание. Превью не затирает список файлов (он в отдельной панели)."""
        self._open_artifacts()
        if kind == "image":
            self.artifact_panel.clear()
            with self.artifact_panel:
                with ui.card().classes("sov-artifact-card"):
                    _html(f'<div class="sov-panel-title">{esc(name)}</div>')
                    ui.image(url).style("width:100%;border-radius:6px;")
            return
        if kind in ("xlsx", "csv"):
            res = await api_get_bytes(url)
            if not res:
                ui.notify(last_api_error_text("Не удалось открыть файл"), type="negative")
                return
            data, _fn = res
            rows = _rows_from_spreadsheet(data, kind)
            self.artifact_panel.clear()
            with self.artifact_panel:
                with ui.card().classes("sov-artifact-card"):
                    with ui.row().classes("w-full items-center justify-between"):
                        _html(f'<div class="sov-panel-title">{esc(name)}</div>')
                        ui.button("Скачать", icon="o_download",
                                  on_click=lambda u=url, n=name: _download_file_artifact(u, n)
                                  ).props("no-caps flat dense")
                    if rows:
                        _render_table(rows)
                    else:
                        ui.markdown("_Пустой файл или не удалось разобрать таблицу._").classes("sov-artifact-markdown")
            return
        await _download_file_artifact(url, name)


    def _register_file_artifact(self, name: str, url: str, kind: str = "file") -> None:
        if not url or url in self._file_artifacts:
            return
        self._open_artifacts()
        self._file_artifacts[url] = {"name": name, "kind": kind}
        self.files_artifacts_panel.set_visibility(True)
        with self.files_artifacts_panel:
            if len(self._file_artifacts) == 1:
                _html('<div class="sov-panel-title" style="margin-top:6px;">Файлы</div>')
            icon = {"xlsx": "o_table_chart", "csv": "o_grid_on", "docx": "o_description",
                    "image": "o_image", "pdf": "o_picture_as_pdf"}.get(kind, "o_insert_drive_file")
            with ui.card().classes("sov-file-card"):
                with ui.row().classes("w-full items-center gap-2 no-wrap"):
                    ui.icon(icon).classes("sov-file-icon")
                    ui.label(name).classes("sov-file-name").style(
                        "flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;font-size:.72rem;"
                    )
                    ui.button(icon="o_visibility",
                              on_click=lambda u=url, n=name, k=kind: self._preview_file_artifact(u, n, k)
                              ).props("flat round dense").tooltip("Открыть в панели")
                    ui.button(icon="o_download",
                              on_click=lambda u=url, n=name: _download_file_artifact(u, n)
                              ).props("flat round dense").tooltip("Скачать")


    def _register_artifact_downloads(self, meta: dict | None) -> None:
        artifact = (meta or {}).get("artifact") if isinstance(meta, dict) else {}
        if not isinstance(artifact, dict):
            return
        for item in artifact_workbook_files(artifact):
            self._register_file_artifact(
                item["filename"], item["download_url"], "xlsx"
            )
        downloads = artifact.get("downloads") or {}
        if not isinstance(downloads, dict):
            return
        title = str(artifact.get("title") or "Сметный артефакт").strip() or "Сметный артефакт"
        for ext, label in (("xlsx", "Excel"), ("csv", "CSV")):
            url = str(downloads.get(ext) or "").strip()
            if url:
                self._register_file_artifact(f"{title} · {label}.{ext}", url, ext)


    def _clear_file_artifacts(self) -> None:
        self._file_artifacts.clear()
        self.files_artifacts_panel.clear()
        self.files_artifacts_panel.set_visibility(False)


    def _html_set_artifact_mode(self, label: str, hint: str):
        self._open_artifacts()
        self.artifact_panel.clear()
        with self.artifact_panel:
            _html(
                '<div class="sov-artifact-empty">'
                f'<div class="sov-artifact-empty-title">{esc(label)}</div>'
                f'<div class="sov-muted">{esc(hint)}</div>'
                '</div>'
            )


    def _render_artifact_loading(self, mode: str, question: str):
        self._open_artifacts()
        self.artifact_panel.clear()
        label = OUTPUT_FORMATS.get(mode, ("Артефакт", ""))[0]
        with self.artifact_panel:
            _html(
                '<div class="sov-artifact-empty">'
                f'<div class="sov-artifact-empty-title">{esc(label)}</div>'
                f'<div class="sov-muted">Готовлю артефакт по запросу: {esc(question[:100])}</div>'
                '<div class="sov-artifact-loader"></div>'
                '</div>'
            )


    def _render_artifact_error(self, detail: str):
        detail = str(detail or "").strip()
        if len(detail) > 1200:
            detail = detail[:1200].rstrip() + "…"
        self._open_artifacts()
        self.artifact_panel.clear()
        with self.artifact_panel:
            _html(
                '<div class="sov-artifact-empty">'
                '<div class="sov-artifact-empty-title" style="color:var(--err);">Ошибка</div>'
                f'<div class="sov-muted">{esc(detail)}</div>'
                '</div>'
            )


    def _render_result(self, ans: str, mode: str, container, table_query: dict | None = None):
        container.clear()
        with container:
            with ui.card().classes("sov-artifact-card"):
                label = "Интерактивная таблица" if table_query else OUTPUT_FORMATS.get(mode, ("Ответ", ""))[0]
                if mode == "markdown":
                    label = "Инженерный блокнот"
                # text-режим с таблицей внутри → заголовок «Таблица», а не «Текст».
                # markdown-артефакты показываем целиком: это блокнот/отчёт, а не первая таблица.
                if not table_query and mode == "text" and (_parse_table_from_ai(ans) or _parse_markdown_table(ans)):
                    label = "Таблица"
                with ui.row().classes("w-full items-center justify-between"):
                    _html(f'<div class="sov-panel-title">{esc(label)}</div>')
                    _copy_button("Копировать", ans, props="no-caps flat dense")

                if table_query:
                    _render_table_query(table_query)
                elif mode == "text":
                    # Если в ответе есть таблица — артефакт = ТОЛЬКО таблица + CSV (прозу
                    # видно в чате; Олег: «артефакт только таблицу, текст я и так вижу»).
                    auto = _parse_table_from_ai(ans) or _parse_markdown_table(ans)
                    if isinstance(auto, list) and auto and isinstance(auto[0], dict):
                        _render_table(auto)
                    else:
                        ui.markdown(ans).classes("sov-artifact-markdown")
                elif mode == "markdown":
                    ui.markdown(ans).classes("sov-artifact-markdown")
                elif mode == "spec":
                    data = _parse_table_from_ai(ans)
                    if isinstance(data, list) and data and isinstance(data[0], dict):
                        _render_gost_spec(data)
                    else:
                        ui.markdown(ans).classes("sov-artifact-markdown")
                elif mode == "verify":
                    from sovushka.pages.verify import render_verify_artifact
                    render_verify_artifact(_parse_json_from_ai(ans))
                elif mode == "schema":
                    data = _parse_json_from_ai(ans)
                    if data:
                        with ui.column().classes("w-full gap-1"):
                            _render_tree(data)
                    else:
                        ui.markdown(ans).classes("sov-artifact-markdown")
                elif mode in ("structure", "table", "template"):
                    data = _parse_table_from_ai(ans) or _parse_json_from_ai(ans)
                    if isinstance(data, list) and data:
                        _render_table(data if isinstance(data[0], dict) else [{"значение": str(r)} for r in data])
                    elif isinstance(data, dict):
                        ui.markdown(f"```json\n{json.dumps(data, ensure_ascii=False, indent=2)}\n```").classes("sov-artifact-markdown")
                    else:
                        ui.markdown(ans).classes("sov-artifact-markdown")
                elif mode == "mermaid":
                    code = _parse_mermaid_from_ai(ans)
                    if code:
                        state["mermaid_last"] = code
                        ui.mermaid(code).classes("w-full")
                        with ui.row().classes("gap-2"):
                            _copy_button("Код", code, props="no-caps flat dense")
                            if self.tabs and self.tab_mermaid:
                                ui.button("В редактор", icon="o_open_in_new", on_click=lambda: self.tabs.set_value(self.tab_mermaid)).props("no-caps flat dense")
                    else:
                        ui.markdown(ans).classes("sov-artifact-markdown")
                elif mode == "svg":
                    svg_code = _parse_svg_from_ai(ans)
                    if svg_code:
                        _html(f'<div class="sov-svg-preview">{svg_code}</div>')
                        _copy_button("Копировать SVG", svg_code, props="no-caps flat dense")
                    else:
                        ui.markdown(ans).classes("sov-artifact-markdown")
