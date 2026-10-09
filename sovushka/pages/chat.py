"""
С.О.В.У.Ш.К.А. v5.0 — премиальная рабочая вкладка AI ЧАТ
"""
from __future__ import annotations
from sovushka.components.activity import ActivityPanel
from sovushka.components.chat_messages import ChatMessages
from sovushka.components.chat_artifacts import ChatArtifacts, OUTPUT_FORMATS

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

from nicegui import app, context, ui
from sovushka.components.chat_drafts import ChatDrafts

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


def _restore_failed_question(chat_input, drafts, question: str) -> None:
    # Do not overwrite a new question typed while the failed request was running.
    if not str(chat_input.value or "").strip():
        chat_input.set_value(question)
        drafts.save(question)


def _clear_scope_selection(
    selected_projects: set,
    selected_datasets: set,
    checkboxes: list[Any],
) -> None:
    """Clear both the scope state and the checkboxes currently shown in the dialog."""
    selected_projects.clear()
    selected_datasets.clear()
    for checkbox in checkboxes:
        checkbox.set_value(False)


def _is_spec_request(q: str) -> bool:
    """«Собери/составь/сделай … спецификацию …» — намерение собрать спеку → GOST-форма."""
    import re as _re
    return bool(_re.search(r"\b(собери|составь|сделай|сформируй|подготовь)\b.{0,40}специфика",
                           (q or "").lower()))


CHAT_MODE_GUIDANCE = {
    "search": {
        "title": "Поиск по источникам",
        "description": "Ищет ответ в выбранных проектах, датасетах и документах и показывает источники.",
        "data_hint": "Лучше всего: вопрос + объект, раздел, шифр или название документа.",
        "examples": (
            "Где описана система дымоудаления?",
            "Найди требования к пределу огнестойкости",
            "Что сказано о котельной в проекте?",
        ),
    },
    "agent": {
        "title": "Агент-исследователь",
        "description": "Сам выбирает несколько безопасных read-only инструментов и собирает проверяемый ответ.",
        "data_hint": "Может искать в интернете, документах ЛЕС и разрешённых папках; изменения на компьютере не выполняет.",
        "examples": (
            "Найди актуальные разъяснения по этому вопросу в интернете",
            "Найди на компьютере документы по названию объекта",
            "Сравни проектные документы с публичными источниками",
        ),
    },
    "engineer": {
        "title": "Инженер",
        "description": "Проверяет проектные документы и формирует замечания со ссылками на требования.",
        "data_hint": "Выберите комплект или приложите PDF; желательно указать стадию и вид проверки.",
        "examples": (
            "Проверь комплектность проектной документации",
            "Проверь основные надписи на листах",
            "Найди нарушения требований СПДС",
        ),
    },
}


def visible_chat_modes() -> tuple[str, ...]:
    """Modes exposed by the ordinary chat composer."""
    from backend.product_edition import profile_modes
    return profile_modes()


def default_chat_mode() -> str:
    return "agent"


def _split_pasted_context_for_payload(
    question: str,
    *,
    mode: str,
    has_attachment: bool,
) -> tuple[str, str, str]:
    """Длинная вставка в сметном чате: короткая задача отдельно, исходник отдельно.

    Модель должна рассуждать по ВОР/спецификации как по данным, а не таскать весь
    лист Excel внутри поля `question`, где живут роутинг, история и resource gate.
    """
    q = str(question or "").strip()
    if has_attachment or mode != "smeta":
        return q, "", ""
    line_count = q.count("\n") + 1 if q else 0
    if len(q) <= 3600 and line_count < 24:
        return q, "", ""

    paragraphs = re.split(r"\n\s*\n", q, maxsplit=1)
    task = paragraphs[0].strip() if paragraphs else ""
    if len(task) < 20 or len(task) > 1200:
        task = q[:900].strip()
    if not re.search(r"(сделай|составь|посчитай|рассчитай|смет|вор|ведомост|хочу|нужн)", task, re.IGNORECASE):
        task = "Сделай сметный разбор, ВОР и сметную структуру по вставленному исходнику."

    header = "Вставленный исходник из сообщения оператора:\n\n"
    trunc_note = "\n\n[Исходник усечён интерфейсом до лимита контекста; нужен файл или более узкий фрагмент.]"
    max_body = max(0, 20000 - len(header))
    if len(q) > max_body:
        max_body = max(0, 20000 - len(header) - len(trunc_note))
        body = q[:max_body].rstrip() + trunc_note
    else:
        body = q
    context = f"{header}{body}"[:20000]
    suffix = f"📎 Длинный исходник перенесён в контекст сметчика: {len(q)} симв."
    return task, context, suffix


def should_skip_chat_resource_gate(question: str, dataset_filter: str | None = None) -> bool:
    selected_filter = dataset_filter if dataset_filter and dataset_filter != "(все датасеты)" else None
    try:
        from proxy.services.kot_service import analyze_question
        from proxy.services.query_router import route_query

        intent = route_query(question, dataset_filter=selected_filter)
        kot = analyze_question(question)
        effective_filter = selected_filter or intent.dataset_filter or kot.dataset_filter
        return intent.channel in {"mail", "table"} or effective_filter in {"MAIL", "TABLE"}
    except Exception:
        q = question.casefold()
        table_hint = any(token in q for token in ("смет", "таблиц", "строк", "стоимост", "итого"))
        aggregate_hint = any(token in q for token in ("посчитай", "сумм", "сколько", "покажи"))
        mail_hint = any(token in q for token in ("почт", "письм", "email", "mail", "dropbox"))
        return mail_hint or (table_hint and aggregate_hint)


def build_chat(is_admin: bool, tabs=None, tab_mermaid=None, tab_documents=None):
    """Строит автономный экран чата: история слева, чат по центру, артефакты справа."""

    out_mode_val = {"v": default_chat_mode()}
    from sovushka.state import ensure_session_id
    drafts = ChatDrafts(app.storage.user, ensure_session_id())
    apply_active_profile = {"v": False}
    selected_session_card = {"el": None}
    project_state = {"id": None}  # W17.1: активный объект (None = обычный RAG по всему)
    _pending_target_file = {"v": ""}
    _pending_target_files = {"v": []}

    # Резиновый layout: тащим разделитель → меняем ширину панели артефактов (CSS-var),
    # ширина сохраняется в localStorage. Деградирует мягко (нет JS → разделитель статичен).
    ui.add_body_html("""
    <script>
    (function(){
      function init(){
        var shell=document.querySelector('.sov-chat-shell');
        var div=document.querySelector('.sov-resize-divider');
        if(!shell||!div){ setTimeout(init,400); return; }
        if(div.dataset.bound){ return; } div.dataset.bound='1';
        try{ var s=localStorage.getItem('sovArtW'); if(s) shell.style.setProperty('--sov-artifacts-w', s); }catch(e){}
        var drag=false;
        div.addEventListener('pointerdown', function(e){ drag=true; try{div.setPointerCapture(e.pointerId);}catch(_){} e.preventDefault(); });
        window.addEventListener('pointermove', function(e){
          if(!drag) return;
          var r=shell.getBoundingClientRect();
          var w=r.right - e.clientX - 14;
          w=Math.max(280, Math.min(820, w));
          shell.style.setProperty('--sov-artifacts-w', w+'px');
        });
        window.addEventListener('pointerup', function(){
          if(!drag) return; drag=false;
          var w=shell.style.getPropertyValue('--sov-artifacts-w');
          try{ if(w) localStorage.setItem('sovArtW', w.trim()); }catch(e){}
        });
      }
      if(document.readyState!=='loading'){ init(); } else { document.addEventListener('DOMContentLoaded', init); }
    })();
    </script>
    """)

    workspace = ChatWorkspace(
        on_open=lambda record: _activate_workspace_session(record),
        get_session_id=lambda: state.get("session_id"),
        is_busy=lambda: _sending["v"], is_admin=is_admin,
    )
    project_navigation = ChatProjectNavigation(
        workspace, on_new=lambda: _clear_chat(), on_history=lambda: _toggle_history(),
        on_data=(lambda: tabs.set_value(tab_documents)) if tabs is not None and tab_documents is not None else None,
    )
    workspace.on_change = project_navigation.refresh

    with ui.element("div").classes(
        "sov-chat-shell sov-ui-shell sov-chat-workspace sov-forest-auto-sources"
    ) as chat_shell:
        project_navigation.render()
        history_drawer = ui.element("aside").classes("sov-history-drawer")
        history_drawer.set_visibility(False)

        with history_drawer:
            with ui.row().classes("w-full items-center justify-between"):
                _html('<div class="sov-panel-title">История</div>')
                action_button(
                    icon="o_close",
                    on_click=lambda: history_drawer.set_visibility(False),
                    variant="quiet",
                    icon_only=True,
                    aria_label="Закрыть историю",
                    classes="sov-icon-btn",
                )
            sessions_col = ui.column().classes("w-full gap-2 sov-history-list")

        # «Задачи и объёмы» убраны из чата (Олег): ввод — командами, просмотр — в админ-вкладках.

        # W18.1: файл-вьювер — дерево RAG_Content + просмотр (текст/код/картинка/PDF).
        files_drawer = ui.element("aside").classes("sov-history-drawer")
        files_drawer.set_visibility(False)
        with files_drawer:
            with ui.row().classes("w-full items-center justify-between"):
                _html('<div class="sov-panel-title">Файлы</div>')
                action_button(
                    icon="o_close",
                    on_click=lambda: files_drawer.set_visibility(False),
                    variant="quiet",
                    icon_only=True,
                    aria_label="Закрыть",
                    classes="sov-icon-btn",
                )
            files_tree_box = ui.column().classes("w-full gap-0").style("max-height:38%;overflow:auto;")
            ui.separator().style("border-color:var(--border);margin:6px 0;")
            files_view_box = ui.column().classes("w-full gap-1 sov-history-list")
            with files_view_box:
                _html('<div class="sov-muted" style="font-size:.62rem;">Выбери файл в дереве для просмотра.</div>')

        with ui.element("main").classes("sov-chat-main"):
            with ui.row().classes("sov-chat-topbar"):
                with ui.row().classes("items-center gap-2"):
                    action_button("Проекты и чаты", icon="menu", variant="quiet",
                                  classes="sov-project-toggle", on_click=project_navigation.toggle).props('aria-haspopup="dialog"')
                    project_navigation.render_heading()
                with ui.row().classes("sov-workspace-header-actions"):
                    action_button("Память", icon="o_bookmark_border", variant="quiet",
                                  on_click=lambda: workspace.run(workspace.open_memory))
                    action_button("Файлы", icon="o_description", variant="quiet",
                                  on_click=lambda: artifacts._open_artifacts())
                    # v0.22 ScopeSelector — ОБЛАСТЬ ПОИСКА (весь RAG / проект(ы) / датасет(ы) / mixed).
                    # Заменяет неясную выпадашку: явные группы Проекты/Датасеты/Непривязанные/Системные.
                    scope_state = {
                        "scope_type": "none",
                        "project_ids": [],
                        "dataset_ids": [],
                        "label": "Без источников",
                        "selected_sources_only": False,
                    }
                    scope_opts_cache: dict = {"data": None}

                    scope_btn = action_button(
                        "Без источников",
                        icon="o_travel_explore",
                        variant="secondary",
                        compact=True,
                        classes="sov-scope-btn",
                    ).tooltip(
                        "Область поиска: в каких проектах и датасетах ЛЕС будет искать источники.")
                    with ui.column().classes("w-full gap-2") as search_controls:
                        selected_sources_only_switch = ui.switch(
                            "Только выбранные источники",
                            value=bool(scope_state["selected_sources_only"]),
                        ).props("dense")
                        selected_sources_only_switch.tooltip(
                            "Отключает публичный веб-поиск для этого диалога; выбор датасета сам по себе веб не запрещает."
                        )
                        selected_sources_only_switch.on_value_change(
                            lambda event: scope_state.__setitem__(
                                "selected_sources_only", bool(event.value)
                            )
                        )
                        reranker_checkbox = checkbox_field("Уточнять порядок источников · реранкер", value=False).props("color=primary")
                        reranker_checkbox.tooltip(
                            "Локальная модель повторно оценивает найденные фрагменты. Медленнее обычного поиска. Можно отключить."
                        )
                        reranker_status = ui.label("Проверяем доступность реранкера…").classes("text-sm text-secondary")
                        reranker_status.props('role="status" aria-live="polite"')
                        reranker_checkbox.disable()

                        async def _refresh_reranker_status():
                            status = await api_get("/api/rerank/status") or {}
                            reranker_checkbox.set_enabled(bool(status.get("available")))
                            reranker_status.set_text(str(status.get("detail") or "Не удалось проверить реранкер. Обычный поиск доступен."))

                        ui.timer(0.1, _refresh_reranker_status, once=True)
                    search_controls.set_visibility(False)

                    def _scope_label() -> str:
                        st = scope_state["scope_type"]
                        np, nd = len(scope_state["project_ids"]), len(scope_state["dataset_ids"])
                        data = scope_opts_cache["data"] or {}
                        if st == "all":
                            return "Все источники"
                        if st == "none":
                            return "Без источников"
                        if st == "project" and np == 1:
                            for p in data.get("projects", []):
                                if int(p["id"]) == scope_state["project_ids"][0]:
                                    return str(p["name"])
                        if st == "dataset" and nd == 1:
                            for d in data.get("datasets", []) + data.get("system_datasets", []):
                                if str(d["id"]) == scope_state["dataset_ids"][0]:
                                    return str(d["name"])[:28]
                        if st == "projects":
                            return f"{np} проекта · {nd} датасетов"
                        if st == "datasets":
                            return f"{nd} датасета"
                        if st == "mixed":
                            return "Смешанная область"
                        return "Все источники"

                    def _apply_scope(sel_projects: set, sel_datasets: set) -> None:
                        scope_state["project_ids"] = sorted(sel_projects)
                        scope_state["dataset_ids"] = sorted(sel_datasets)
                        np, nd = len(sel_projects), len(sel_datasets)
                        if np == 0 and nd == 0:
                            scope_state["scope_type"] = "none"
                        elif np and nd:
                            scope_state["scope_type"] = "mixed"
                        elif np == 1:
                            scope_state["scope_type"] = "project"
                        elif np > 1:
                            scope_state["scope_type"] = "projects"
                        elif nd == 1:
                            scope_state["scope_type"] = "dataset"
                        else:
                            scope_state["scope_type"] = "datasets"
                        # back-compat: одиночный проект → project_state (карта объекта и пр.)
                        project_state["id"] = workspace.active.get("project_id")
                        scope_state["label"] = _scope_label()
                        scope_btn.set_text(scope_state["label"])
                        try:
                            asyncio.create_task(_refresh_scope_files_panel())
                        except NameError:
                            pass

                    def _apply_all_scope() -> None:
                        scope_state["project_ids"] = []
                        scope_state["dataset_ids"] = []
                        scope_state["scope_type"] = "all"
                        scope_state["label"] = _scope_label()
                        project_state["id"] = workspace.active.get("project_id")
                        scope_btn.set_text(scope_state["label"])
                        try:
                            asyncio.create_task(_refresh_scope_files_panel())
                        except NameError:
                            pass

                    async def _load_scope_options() -> dict:
                        data = await api_get("/api/scope/options")
                        if isinstance(data, dict) and (
                            data.get("projects") or data.get("datasets") or data.get("unassigned_datasets") or data.get("system_datasets")
                        ):
                            return data

                        projects_payload = await api_get("/api/projects") or {}
                        datasets_payload = await api_get("/api/rag/datasets") or []
                        projects_raw = (
                            projects_payload.get("projects", [])
                            if isinstance(projects_payload, dict) else
                            projects_payload if isinstance(projects_payload, list) else []
                        )
                        datasets_raw = (
                            datasets_payload
                            if isinstance(datasets_payload, list) else
                            datasets_payload.get("datasets") or datasets_payload.get("value") or []
                            if isinstance(datasets_payload, dict) else []
                        )
                        datasets = [
                            {
                                "id": str(d.get("id", "")),
                                "name": str(d.get("name") or d.get("id") or ""),
                                "source_type": str(d.get("group_name") or "dataset"),
                                "file_count": int(d.get("files", d.get("doc_count", 0)) or 0),
                                "sidecar_status": "unknown",
                                "lexical_status": "unknown",
                                "qdrant_status": "indexed" if int(d.get("chunk_count", d.get("chunks", 0)) or 0) else "unknown",
                                "project_ids": [],
                            }
                            for d in datasets_raw
                            if isinstance(d, dict) and d.get("id")
                        ]
                        projects = [
                            {
                                "id": int(p.get("id", 0)),
                                "name": str(p.get("name") or p.get("id") or ""),
                                "aliases": p.get("aliases") or [],
                                "dataset_count": int(p.get("dataset_count", p.get("datasets", 0)) or 0),
                                "dataset_ids": p.get("dataset_ids") or [],
                                "dataset_roles": p.get("dataset_roles") or [],
                                "warnings": p.get("warnings") or [],
                            }
                            for p in projects_raw
                            if isinstance(p, dict) and p.get("id")
                        ]
                        return {
                            "all": {"scope_type": "all", "label": "Весь RAG"},
                            "projects": projects,
                            "datasets": datasets,
                            "unassigned_datasets": datasets,
                            "system_datasets": [],
                            "counts": {
                                "projects_total": len(projects),
                                "datasets_total": len(datasets),
                                "datasets_unassigned": len(datasets),
                                "datasets_system": 0,
                            },
                        }

                    async def _prefetch_scope(force: bool = False) -> dict:
                        if scope_opts_cache.get("data") and not force:
                            return scope_opts_cache["data"]
                        scope_opts_cache["data"] = await _load_scope_options()
                        return scope_opts_cache["data"]

                    def _open_scope_dialog():
                        # СИНХРОННО строим диалог из prefetch-кэша (как version-диалог): создавать UI в
                        # фоновом asyncio-таске нельзя (slot stack empty). Данные тянет _prefetch_scope.
                        data = scope_opts_cache["data"] or {}
                        sel_p = set(scope_state["project_ids"]); sel_d = set(scope_state["dataset_ids"])
                        scope_checkboxes: list[Any] = []
                        with ui.dialog() as dlg, panel(variant="raised", classes="sov-ui-dialog"):
                            section_heading("Область поиска", "Выберите источники, которые увидит модель")
                            if not data.get("projects") and not data.get("datasets"):
                                ui.label("Список проектов и датасетов пока не загрузился.").classes("sov-muted")
                                async def _reload_scope_dialog():
                                    await _prefetch_scope(force=True)
                                    dlg.close()
                                    _open_scope_dialog()
                                action_button("Обновить список", icon="o_refresh", on_click=_reload_scope_dialog)
                            search = text_field(label="Поиск", placeholder="По проектам и базам", clearable=True, classes="w-full")
                            selection_note = ui.label(
                                f"Выбрано: {len(sel_p) + len(sel_d)}"
                            ).classes("sov-scope-selection-note")

                            def _update_selection_note() -> None:
                                selection_note.set_text(f"Выбрано: {len(sel_p) + len(sel_d)}")

                            def _dataset_title(item: dict) -> str:
                                return str(item.get("display_name") or item.get("name") or item.get("id") or "Источник")

                            def _dataset_meta(item: dict) -> str:
                                files = int(item.get("file_count") or 0)
                                chunks = int(item.get("chunk_count") or 0)
                                parts = [f"{files} файлов"]
                                if chunks:
                                    parts.append(f"{chunks:,}".replace(",", " ") + " фрагментов")
                                if str(item.get("dataset_scope") or "") == "system":
                                    parts.append("база ЛЕС")
                                return " · ".join(parts)

                            with ui.scroll_area().classes("sov-ui-dialog-scroll"):
                                def _row_match(name: str) -> bool:
                                    q = (search.value or "").strip().lower()
                                    return not q or q in name.lower()

                                def _cb(label, key, store, sub="", *, icon="o_dataset", disabled=False):
                                    card_classes = "sov-scope-option-card"
                                    if disabled:
                                        card_classes += " sov-scope-option-disabled"
                                    with ui.element("div").classes(card_classes):
                                        ui.icon(icon).classes("sov-scope-option-icon")
                                        with ui.column().classes("sov-scope-option-copy"):
                                            ui.label(label).classes("sov-scope-option-title")
                                            if sub:
                                                ui.label(sub).classes("sov-scope-option-meta")
                                        cb = checkbox_field("", value=key in store).props(f'aria-label={json.dumps(label)}')

                                    def _toggle(e, k=key, s=store):
                                        s.add(k) if e.args else s.discard(k)
                                        _update_selection_note()

                                    cb.on("update:model-value", _toggle)
                                    if disabled:
                                        cb.disable()
                                    scope_checkboxes.append(cb)
                                    return cb

                                ui.label("ПРОЕКТЫ").classes("sov-scope-section-title")
                                for p in data.get("projects", []):
                                    if _row_match(str(p["name"])):
                                        count = int(p.get("dataset_count") or 0)
                                        sub = f"{count} датасетов" if count else "Нет подключённых датасетов"
                                        _cb(str(p["name"]), int(p["id"]), sel_p, sub=sub,
                                            icon="o_workspaces", disabled=not count)
                                _unassigned_ids = {str(d["id"]) for d in data.get("unassigned_datasets", [])}
                                assigned = [d for d in data.get("datasets", []) if str(d["id"]) not in _unassigned_ids]
                                if assigned:
                                    ui.label("ДАТАСЕТЫ ПРОЕКТОВ").classes("sov-scope-section-title")
                                    for d in assigned:
                                        if _row_match(str(d["name"])):
                                            _cb(_dataset_title(d), str(d["id"]), sel_d,
                                                sub=_dataset_meta(d), icon="o_folder_copy")
                                if data.get("unassigned_datasets"):
                                    ui.label("ОТДЕЛЬНЫЕ ИСТОЧНИКИ").classes("sov-scope-section-title")
                                    for d in data.get("unassigned_datasets", []):
                                        if _row_match(str(d["name"])):
                                            _cb(_dataset_title(d), str(d["id"]), sel_d,
                                                sub=_dataset_meta(d), icon="o_description")
                                if data.get("system_datasets"):
                                    ui.label("БАЗЫ ЛЕС").classes("sov-scope-section-title")
                                    for d in data.get("system_datasets", []):
                                        available = bool(int(d.get("file_count") or 0)) or str(d.get("qdrant_status")) == "indexed"
                                        meta = _dataset_meta(d) if available else "Пока нет загруженных данных"
                                        _cb(_dataset_title(d), str(d["id"]), sel_d,
                                            sub=meta, icon="o_auto_stories", disabled=not available)
                            with ui.row().classes("sov-scope-dialog-actions"):
                                action_button("Все источники", on_click=lambda: (_apply_all_scope(), dlg.close()), variant="quiet")
                                action_button(
                                    "Очистить выбор",
                                    on_click=lambda: (
                                        _clear_scope_selection(sel_p, sel_d, scope_checkboxes),
                                        _update_selection_note(),
                                    ),
                                    variant="quiet",
                                )
                                action_button("Применить выбор", icon="o_check", on_click=lambda: (_apply_scope(sel_p, sel_d), dlg.close()), variant="primary")
                        dlg.open()

                    async def _scope_click():
                        if not scope_opts_cache.get("data"):
                            scope_btn.props("loading")
                            await _prefetch_scope(force=True)
                            scope_btn.props(remove="loading")
                        _open_scope_dialog()

                    scope_btn.on("click", _scope_click)
                    asyncio.create_task(_prefetch_scope())

                    # Граф знаний: /classic?scope=p:ID|ds:ID — предвыбор области поиска (2×клик в графе).
                    try:
                        _sc = (context.client.request.query_params.get("scope") or "").strip()
                        if _sc.startswith("p:") and _sc[2:].isdigit():
                            _apply_scope({int(_sc[2:])}, set())
                        elif _sc.startswith("ds:") and _sc[3:]:
                            _apply_scope(set(), {_sc[3:]})
                        _tf = (context.client.request.query_params.get("target_file") or "").strip()
                        if _tf:
                            _pending_target_file["v"] = _tf[:512]
                        _tfs = (context.client.request.query_params.get("target_files") or "").strip()
                        if _tfs:
                            try:
                                decoded = json.loads(_tfs)
                            except json.JSONDecodeError:
                                decoded = []
                            if isinstance(decoded, list):
                                _pending_target_files["v"] = [str(x)[:1000] for x in decoded if str(x).strip()][:20]
                    except Exception:
                        pass
                    # Служебные статусы нужны действующему UI-контракту, но не конкурируют с задачей
                    # пользователя. Они остаются доступными коду и техническим деталям ответа.
                    with ui.row().classes("sov-technical-status") as technical_status:
                        mode_chip = ui.label("RAG").classes("sov-chip")
                        validation_chip = ui.label("CRAG ON").classes("sov-chip")
                        model_chip = ui.label("МОДЕЛЬ —").classes("sov-chip sov-model-chip")
                    technical_status.set_visibility(False)

            scope_files_panel = ui.element("div").classes("sov-scope-files-panel")
            scope_files_panel.set_visibility(False)

            # Пока пользователь читает историю выше, новые токены не должны утаскивать
            # его обратно вниз. Возвращаем автопрокрутку только когда он снова у хвоста.
            _chat_follow_tail = {"v": True}

            def _track_chat_scroll(event) -> None:
                remaining = event.vertical_size - event.vertical_position - event.vertical_container_size
                _chat_follow_tail["v"] = remaining <= 48

            def _scroll_chat_to_tail(*, force: bool = False) -> None:
                if force or _chat_follow_tail["v"]:
                    chat_scroll.scroll_to(percent=1)

            chat_scroll = ui.scroll_area(on_scroll=_track_chat_scroll).classes("sov-chat-scroll")
            # The document list scrolls with the conversation, rather than
            # reserving another permanent toolbar above a short answer viewport.
            scope_files_panel.move(chat_scroll)
            with chat_scroll:
                chat_column = ui.column().classes("sov-chat-thread")
                with chat_column:
                    empty_state_ref = {"el": _html(
                        '<div class="sov-chat-empty">'
                        '<div class="sov-chat-empty-title">С чего начнём?</div>'
                        '<div class="sov-chat-empty-copy">Задайте вопрос, приложите документ или выберите источники. ЛЕС поможет разобраться и подготовить результат.</div>'
                        '</div>'
                    )}

            # Скрепка чата: файл к следующему сообщению / быстрая сверка / индексация (W11.8)
            attach_state = {"id": None, "name": "", "mode": "", "text": ""}

            async def _do_attach(e):
                try:
                    upload = getattr(e, "file", None)
                    if upload is not None and hasattr(upload, "read"):
                        content = await upload.read()
                        file_name = getattr(upload, "name", "") or getattr(e, "name", "") or "upload.bin"
                    else:
                        raw = getattr(e, "content", None)
                        if raw is None or not hasattr(raw, "read"):
                            raise AttributeError("upload event has no file/content reader")
                        maybe = raw.read()
                        content = await maybe if inspect.isawaitable(maybe) else maybe
                        file_name = getattr(e, "name", "") or "upload.bin"
                    if isinstance(content, str):
                        content = content.encode("utf-8")
                    if not content:
                        raise ValueError("empty upload content")
                except Exception as error:
                    attach_status.set_text(f"Не удалось прочитать файл: {error}")
                    ui.notify("Не удалось прочитать файл", type="negative")
                    return
                attach_status.set_text(f"Загрузка «{file_name}»…")
                add_log(f"[СКРЕПКА] {file_name} mode={attach_mode.value}")
                d = await api_post_file("/api/rag/attach", content, file_name, params={"mode": attach_mode.value})
                if not isinstance(d, dict):
                    ui.notify(last_api_error_text("Не удалось прикрепить файл"), type="negative")
                    attach_status.set_text(last_api_error_text("Не удалось прикрепить файл"))
                    return
                await _accept_attachment(d)
                picker = getattr(e, 'sender', None)
                if picker is not None and hasattr(picker, 'reset'): picker.reset()

            async def _accept_attachment(d):
                file_name = str(d.get('name') or 'Вложение')
                attach_state.update({
                    "id": d.get("attachment_id"),
                    "name": d.get("name", file_name),
                    "mode": d.get("mode"),
                    "text": d.get("text", ""),
                    "chars": d.get("chars") or len(str(d.get("text") or "")),
                    "rows": d.get("rows") or 0,
                    "truncated": bool(d.get("truncated")),
                    "dataset_name": d.get("dataset_name") or "",
                })
                title, detail, chat_msg = _attachment_visible_text(d)
                if d.get('kind') == 'folder' and d.get('mode') == 'read':
                    title = 'Папка прикреплена к следующему сообщению'
                    detail = f"{file_name} · документов: {d.get('file_count', 0)} · только чтение"
                attach_title.set_text(title)
                attach_chip.set_text(detail)
                attach_strip.set_visibility(True)
                attach_status.set_text("Готово: файл виден под полем ввода")
                attach_dialog.close()
                state["chat_history"].append({
                    "role": "system",
                    "text": chat_msg,
                    "meta": {
                        "attachment": {
                            "id": attach_state.get("id"),
                            "name": attach_state.get("name"),
                            "mode": attach_state.get("mode"),
                            "chars": attach_state.get("chars"),
                            "rows": attach_state.get("rows"),
                            "dataset_name": attach_state.get("dataset_name"),
                        }
                    },
                })
                with chat_column:
                    _render_msg(state["chat_history"][-1])
                _scroll_chat_to_tail(force=True)
                ui.notify(title, type="positive")
                if d.get("mode") == "quick":
                    ui.notify("Таблица пойдёт в scope следующего запроса", type="info")
                elif d.get("mode") == "read":
                    ui.notify("Файл будет отправлен модели вместе со следующим сообщением", type="info")

            async def _drop_attach(event):
                attach_mode.set_value('read')
                await _do_attach(event)

            with ui.dialog() as attach_dialog, ui.card().classes("sov-attach-dialog"):
                with ui.row().classes("sov-attach-dialog__head"):
                    with ui.column().classes("sov-attach-dialog__copy"):
                        ui.label("Добавить файл").classes("sov-attach-dialog__title")
                        ui.label(
                            "Выберите задачу, затем файл. ЛЕС покажет результат под полем запроса."
                        ).classes("sov-attach-dialog__intro")
                    action_button(
                        icon="o_close",
                        on_click=attach_dialog.close,
                        variant="quiet",
                        icon_only=True,
                        aria_label="Закрыть",
                        classes="sov-attach-dialog__close",
                    )

                _ATTACH_MODE_DETAILS = {
                    "read": "Файл один раз передаётся модели со следующим вопросом и не добавляется в базу.",
                    "quick": "XLSX или CSV временно открывается для вопросов, проверки и сравнения строк.",
                    "index": "Документ сохраняется в наборе «Вложения чата» и становится источником ЛЕС.",
                }
                ui.label("Что сделать с файлом?").classes("sov-attach-dialog__label")
                attach_mode_detail = ui.label(_ATTACH_MODE_DETAILS["read"]).classes(
                    "sov-attach-mode-detail"
                )

                def _set_attach_mode_detail(event) -> None:
                    attach_mode_detail.set_text(
                        _ATTACH_MODE_DETAILS.get(str(event.value), _ATTACH_MODE_DETAILS["read"])
                    )

                attach_mode = ui.radio(
                    {
                        "read": "Задать вопрос по файлу",
                        "quick": "Сверить таблицу",
                        "index": "Сохранить в базе ЛЕС",
                    },
                    value="read",
                    on_change=_set_attach_mode_detail,
                ).props("color=primary").classes("sov-attach-mode-picker")
                attach_status = ui.label("").classes("sov-attach-status")
                ui.upload(
                    label="Выбрать файл",
                    auto_upload=True,
                    max_files=1,
                    on_upload=_do_attach,
                ).props("flat accept=.xlsx,.xls,.csv,.pdf,.docx,.txt,.md,.png,.jpg,.jpeg,.webp").classes(
                    "sov-chat-file-picker"
                )

            def _clear_attachment(*, notify: bool = True):
                attach_state.clear()
                attach_state.update({"id": None, "name": "", "mode": "", "text": ""})
                attach_title.set_text("")
                attach_chip.set_text("")
                attach_strip.set_visibility(False)
                if notify:
                    ui.notify("Вложение снято", type="info")

            with ui.element("div").classes("sov-composer") as composer_box:
                from sovushka.components.chat_failure_notice import ChatFailureNotice
                failure_notice = ChatFailureNotice()
                composer_box.on('dragenter', js_handler="""event => {
                    if (Array.from(event.dataTransfer?.types || []).includes('Files'))
                        event.currentTarget.classList.add('sov-composer--dragging');
                }""")
                composer_box.on('dragleave', js_handler="""event => {
                    if (!event.currentTarget.contains(event.relatedTarget))
                        event.currentTarget.classList.remove('sov-composer--dragging');
                }""")
                composer_box.on('drop', js_handler="event => event.currentTarget.classList.remove('sov-composer--dragging')")
                indexing_banner = ui.label("").classes("sov-indexing-banner")
                indexing_banner.set_visibility(False)
                chat_input = ui.textarea(
                    placeholder="Напишите задачу для ЛЕС…", value=drafts.read(),
                    on_change=lambda event: drafts.save(event.value),
                ).classes("sov-composer-input").props('rows=1 autogrow borderless maxlength=20000 aria-label="Ваш запрос"')
                try:
                    preset_question = (context.client.request.query_params.get("question") or "").strip()
                    if preset_question:
                        chat_input.value = preset_question[:4000]
                except Exception:
                    pass
                with ui.row().classes("sov-attachment-strip") as attach_strip:
                    ui.icon("o_attach_file").classes("sov-attachment-icon")
                    with ui.column().classes("sov-attachment-copy"):
                        attach_title = ui.label("").classes("sov-attachment-title")
                        attach_chip = ui.label("").classes("sov-attachment-chip")
                    ui.button(icon="o_close", on_click=lambda: _clear_attachment()).props(
                        'flat round dense aria-label="Снять вложение"'
                    ).classes("sov-icon-btn").tooltip("Снять вложение")
                attach_strip.set_visibility(False)

                ui.upload(label='Перетащите файл сюда или нажмите для выбора', auto_upload=True,
                          max_files=1, on_upload=_drop_attach,
                          on_rejected=lambda _: ui.notify('Добавьте один файл или подключите папку.', type='warning')) \
                    .props('flat accept=.xlsx,.xls,.csv,.pdf,.docx,.txt,.md,.png,.jpg,.jpeg,.webp') \
                    .classes('sov-chat-file-picker sov-composer-drop')

                # Режим только направляет модель к подходящему workflow. Подсказки объясняют
                # ожидаемые данные, но не превращаются в шаблон ответа или keyword-gate.
                _MODE_OPTIONS = {
                    "search": "Поиск",
                    "agent": "Агент",
                    "engineer": "Инженер",
                }
                _MODE_OPTIONS = {key: value for key, value in _MODE_OPTIONS.items() if key in visible_chat_modes()}
                _mode_hint_refs: dict = {}

                def _set_mode(m: str) -> None:
                    if m not in _MODE_OPTIONS:
                        return
                    out_mode_val["v"] = m
                    for _k, _panel in _mode_hint_refs.items():
                        _panel.set_visibility(_k == m)

                def _fill_prompt(text: str) -> None:
                    chat_input.value = text
                    chat_input.update()

                with ui.element("div").classes("sov-composer-footer"):
                    with ui.row().classes("sov-composer-actions"):
                        action_button(
                            "Прикрепить", icon="o_attach_file",
                            on_click=lambda: attach_dialog.open(),
                            variant="quiet",
                            aria_label="Прикрепить файл",
                            classes="sov-composer-action sov-attach-btn",
                        ).tooltip("Прикрепить файл")
                        from sovushka.components.chat_folder import open_chat_folder
                        action_button('Папка', icon='o_create_new_folder', variant='quiet',
                                      on_click=lambda: open_chat_folder(_accept_attachment),
                                      classes='sov-composer-action', aria_label='Добавить папку для чата')
                        response_settings_btn = action_button(
                            "Настройки", icon="o_tune",
                            variant="quiet",
                            on_click=lambda: response_settings_dialog.open(),
                            aria_label="Настройки ответа",
                            classes="sov-response-settings-btn",
                        )
                        with ui.dialog() as response_settings_dialog, panel(variant="raised", classes="sov-ui-dialog") as response_settings_panel:
                            section_heading("Настройки ответа")
                            mode_select = select_field(
                                _MODE_OPTIONS,
                                value=out_mode_val["v"],
                                label="Режим",
                                on_change=lambda event: _set_mode(str(event.value)),
                                aria_label="Режим работы",
                                classes="sov-mode-select",
                            )

                            response_length_select = select_field(
                                {
                                    "short": "Короткий",
                                    "standard": "Обычный",
                                    "detailed": "Подробный",
                                    "maximum": "Максимальный",
                                },
                                value="standard",
                                label="Длина ответа",
                                classes="sov-response-length-select",
                            )
                            with ui.expansion("Примеры запросов", icon="o_lightbulb", value=False).classes(
                                "sov-mode-guidance-disclosure"
                            ):
                                for _mode_key, _guide in CHAT_MODE_GUIDANCE.items():
                                    with ui.element("div").classes("sov-mode-guide") as _guide_panel:
                                        with ui.row().classes("sov-mode-guide-head"):
                                            ui.label(str(_guide["title"])).classes("sov-mode-guide-title")
                                            ui.label(str(_guide["description"])).classes("sov-mode-guide-copy")
                                        ui.label(str(_guide["data_hint"])).classes("sov-mode-data-hint")
                                        with ui.row().classes("sov-mode-examples"):
                                            for _example in _guide["examples"]:
                                                action_button(
                                                    str(_example),
                                                    on_click=lambda _event, example=_example: _fill_prompt(str(example)),
                                                    variant="quiet", classes="sov-mode-example",
                                                )
                                    _guide_panel.set_visibility(_mode_key == out_mode_val["v"])
                                    _mode_hint_refs[_mode_key] = _guide_panel
                            action_button(
                                "Применить активную версию",
                                icon="o_sync",
                                on_click=lambda: (
                                    apply_active_profile.__setitem__("v", True),
                                    ui.notify(
                                        "Активная версия применится к следующему сообщению",
                                        type="info",
                                    ),
                                ),
                                variant="quiet",
                                compact=True,
                                classes="sov-apply-profile-action",
                            )
                            section_heading("Поиск")
                            search_controls.move(response_settings_panel)
                            search_controls.set_visibility(True)
                            action_button("Готово", variant="primary", on_click=response_settings_dialog.close,
                                          classes="self-end")
                        stop_dialog_btn = ui.button(
                            "Остановить диалог",
                            icon="o_stop_circle",
                            on_click=lambda: _stop_active_dialog(),
                        ).props('no-caps flat aria-label="Остановить текущий ответ"').classes(
                            "sov-stop-dialog-btn"
                        ).tooltip("Остановить текущий ответ; история диалога сохранится")
                        stop_dialog_btn.set_visibility(False)
                        send_btn = action_button(
                            "Отправить",
                            icon="o_send",
                            on_click=lambda: asyncio.create_task(send_chat()),
                            variant="primary",
                            aria_label="Отправить",
                            classes="sov-send-btn",
                        )
                ui.label("Enter — отправить · Shift+Enter — новая строка").classes("sov-composer-key-hint")

        # Резиновый layout: разделитель между чатом и артефактами (таскать по ширине).
        artifact_divider = ui.element("div").classes("sov-resize-divider")
        artifact_divider.set_visibility(True)

        with ui.element("aside").classes("sov-artifacts-panel") as artifact_shell:
            artifact_shell.set_visibility(True)
            with ui.row().classes("w-full items-center justify-between"):
                _html('<div class="sov-panel-title">Источники и файлы</div>')
                ui.button(
                    icon="o_close",
                    on_click=lambda: artifacts._set_artifacts_visible(False),
                ).props('flat round dense aria-label="Закрыть источники и файлы"').classes("sov-icon-btn")
            artifact_panel = ui.column().classes("sov-artifacts-body")
            with artifact_panel:
                _html(
                    '<div class="sov-artifact-empty">'
                    '<div class="sov-artifact-empty-title">Ответ можно проверить</div>'
                    '<div class="sov-muted">Выберите источники для вопроса. Здесь появятся материалы ответа и файлы, которые можно открыть.</div>'
                    '</div>'
                )
            # Готовые ФАЙЛЫ-артефакты (сметы xlsx, документы форм): отдельная панель,
            # её _render_result не чистит — список накапливается за сессию.
            files_artifacts_panel = ui.column().classes("sov-files-artifacts")
            files_artifacts_panel.set_visibility(False)

    with ui.dialog() as advanced_dialog:
        with panel(variant="raised", classes="sov-ui-dialog sov-advanced-dialog"):
            with ui.row().classes("w-full items-center justify-between"):
                with ui.column().classes("gap-0"):
                    _html('<div class="sov-panel-title">Расширенный запрос</div>')
                    _html('<div class="sov-muted">формат, датасет, стиль и образец выдачи</div>')
                ui.button(icon="o_close", on_click=advanced_dialog.close).props('flat round dense aria-label="Закрыть"').classes("sov-icon-btn")

            with ui.scroll_area().classes("sov-advanced-scroll"):
                with ui.column().classes("w-full gap-3"):
                    with ui.card().classes("sov-control-card"):
                        _html('<div class="section-title">Формат выдачи</div>')
                        format_hint_lbl = ui.label(OUTPUT_FORMATS["text"][1]).classes("sov-muted")
                        format_btns = {}
                        with ui.grid(columns=4).classes("w-full gap-2"):
                            for key, (label, hint) in OUTPUT_FORMATS.items():
                                btn = ui.button(label).props("no-caps flat").classes("sov-format-btn")
                                format_btns[key] = btn

                    with ui.card().classes("sov-control-card"):
                        _html('<div class="section-title">Параметры форматов</div>')
                        mermaid_opts_row = ui.column().classes("w-full gap-2")
                        with mermaid_opts_row:
                            mermaid_type = ui.select(
                                [
                                    "flowchart TD",
                                    "flowchart LR",
                                    "sequenceDiagram",
                                    "erDiagram",
                                    "gantt",
                                    "classDiagram",
                                    "mindmap",
                                ],
                                value="flowchart TD",
                                label="Тип диаграммы",
                            ).classes("w-full")
                        mermaid_opts_row.set_visibility(False)

                        svg_opts_row = ui.column().classes("w-full gap-2")
                        with svg_opts_row:
                            svg_type = ui.select(
                                [
                                    "Аксонометрическая схема",
                                    "План помещения",
                                    "Функциональная схема",
                                    "Принципиальная схема",
                                    "Организационная структура",
                                    "Диаграмма потоков",
                                ],
                                value="Функциональная схема",
                                label="Тип SVG",
                            ).classes("w-full")
                            svg_size = ui.select(
                                ["800×600", "1200×800", "600×400", "1600×900"],
                                value="800×600",
                                label="Размер",
                            ).classes("w-full")
                        svg_opts_row.set_visibility(False)

                        spec_opts_row = ui.column().classes("w-full gap-2")
                        with spec_opts_row:
                            spec_type = ui.select(
                                [
                                    "Спецификация оборудования (по ГОСТ 21.110)",
                                    "Ведомость чертежей (ГОСТ 21.101)",
                                    "Ведомость ссылочных документов",
                                    "Спецификация материалов",
                                    "Перечень элементов (ПЭ3)",
                                ],
                                value="Спецификация оборудования (по ГОСТ 21.110)",
                                label="Тип спецификации",
                            ).classes("w-full")
                            spec_group = ui.switch("Группировать по разделам")
                            spec_gost = ui.switch("Строгий формат ГОСТ", value=True)
                        spec_opts_row.set_visibility(False)

                        schema_opts_row = ui.column().classes("w-full gap-2")
                        with schema_opts_row:
                            schema_depth = ui.number("Глубина вложенности", value=3, min=1, max=6, step=1).classes("w-full")
                            schema_format = ui.select(
                                ["JSON дерево", "Маркированный список", "Нумерованный список", "YAML"],
                                value="JSON дерево",
                                label="Формат схемы",
                            ).classes("w-full")
                        schema_opts_row.set_visibility(False)

                        template_row = ui.column().classes("w-full gap-2")
                        with template_row:
                            ui.label("Файл-образец: JSON, CSV или XLSX").classes("sov-muted")
                            ui.upload(
                                auto_upload=True,
                                on_upload=lambda e: asyncio.create_task(load_output_template(e)),
                            ).props("flat accept=.json,.csv,.xlsx").classes("w-full")
                            template_lbl = ui.label("").style("color:var(--ok);font-size:.72rem;")
                            template_preview = _html("").classes("sov-template-preview")
                        template_row.set_visibility(False)

                    with ui.card().classes("sov-control-card"):
                        _html('<div class="section-title">Детали запроса</div>')
                        detail_dataset = ui.select([], label="Датасет").classes("w-full")
                        detail_depth = ui.select(
                            [
                                "Кратко (1-2 абзаца)",
                                "Стандарт (3-5 абзацев)",
                                "Подробно (развёрнутый ответ)",
                                "Максимум (полный анализ)",
                            ],
                            value="Стандарт (3-5 абзацев)",
                            label="Детальность",
                        ).classes("w-full")
                        detail_lang = ui.select(
                            [
                                "Русский (технический)",
                                "Русский (нормативный ГОСТ)",
                                "Краткие тезисы",
                                "Для презентации",
                            ],
                            value="Русский (технический)",
                            label="Стиль",
                        ).classes("w-full")
                        detail_extra = ui.textarea(label="Дополнительные требования").props("rows=3").classes("w-full")

                    with ui.card().classes("sov-control-card"):
                        with ui.row().classes("w-full items-center justify-between"):
                            _html('<div class="section-title">Промпт</div>')
                            ui.button(icon="o_refresh", on_click=lambda: _update_prompt_preview()).props("flat round dense").classes("sov-icon-btn")
                        prompt_preview = _html("").classes("sov-prompt-preview")

            with ui.row().classes("w-full justify-end gap-2"):
                ui.button("Закрыть", on_click=advanced_dialog.close).props("no-caps flat")
                apply_btn = ui.button(
                    "Применить и отправить",
                    icon="o_send",
                    on_click=lambda: asyncio.create_task(send_with_form()),
                ).props("no-caps")

    with ui.dialog() as passport_dialog:
        with ui.card().classes("sov-advanced-dialog"):
            with ui.row().classes("w-full items-center justify-between"):
                with ui.column().classes("gap-0"):
                    ui.label("Блокнот области").classes("sov-panel-title")
                    passport_scope_label = ui.label("Весь RAG").classes("sov-muted")
                ui.button(icon="o_close", on_click=passport_dialog.close).props(
                    'flat round dense aria-label="Закрыть"'
                ).classes("sov-icon-btn")
            with ui.scroll_area().classes("sov-advanced-scroll"):
                passport_body = ui.column().classes("w-full gap-3")
            with ui.row().classes("w-full justify-end gap-2"):
                ui.button("Закрыть", on_click=passport_dialog.close).props("no-caps flat")

    def select_format(key: str):
        out_mode_val["v"] = key
        label, hint = OUTPUT_FORMATS[key]
        format_hint_lbl.set_text(hint)
        for fmt_key, btn in format_btns.items():
            if fmt_key == key:
                btn.classes(add="sov-format-btn-active")
            else:
                btn.classes(remove="sov-format-btn-active")
        mermaid_opts_row.set_visibility(key == "mermaid")
        svg_opts_row.set_visibility(key == "svg")
        spec_opts_row.set_visibility(key == "spec")
        schema_opts_row.set_visibility(key == "schema")
        template_row.set_visibility(key == "template")
        if key == "text":
            artifacts._set_artifacts_visible(False)
        else:
            artifacts._html_set_artifact_mode(label, hint)
        _update_prompt_preview()

    for _key in OUTPUT_FORMATS:
        format_btns[_key].on("click", lambda k=_key: select_format(k))

    async def _load_datasets_select():
        await refresh_samovar()
        names = [s.get("folder", "") for s in state["sources"]]
        detail_dataset.options = ["(все датасеты)"] + names
        detail_dataset.value = "(все датасеты)"
        # W5.7-v2: переход из графа знаний — /classic?dataset=<папка> предвыбирает фильтр.
        try:
            preset = (context.client.request.query_params.get("dataset") or "").strip()
            if preset and preset in names:
                detail_dataset.value = preset
                ui.notify(f"Фильтр из графа: {preset}", type="info")
        except Exception:
            pass
        detail_dataset.update()

    asyncio.create_task(_load_datasets_select())

    def _toggle_history():
        files_drawer.set_visibility(False)
        history_drawer.set_visibility(not history_drawer.visible)
        if history_drawer.visible:
            asyncio.create_task(_load_sessions())

    # «Задачи и объёмы» убраны из чата (Олег) — toggle/refresh-панель не нужны.

    async def _resolve_scope_dataset_ids() -> tuple[list[str], list[str]]:
        ids = [str(x) for x in (scope_state.get("dataset_ids") or []) if str(x)]
        names: list[str] = []
        if scope_state.get("scope_type") in {"none", "all"}:
            return ids, names
        if ids:
            data = scope_opts_cache.get("data") or {}
            by_id = {
                str(d.get("id")): str(d.get("name") or d.get("id"))
                for d in (data.get("datasets", []) + data.get("system_datasets", []) + data.get("unassigned_datasets", []))
            }
            return ids, [by_id.get(i, i) for i in ids]
        payload = {
            "scope": {
                "scope_type": scope_state.get("scope_type"),
                "project_ids": scope_state.get("project_ids") or [],
                "dataset_ids": scope_state.get("dataset_ids") or [],
            }
        }
        resolved = await api_post("/api/scope/resolve", payload)
        if not isinstance(resolved, dict):
            return [], []
        return (
            [str(x) for x in (resolved.get("resolved_dataset_ids") or []) if str(x)],
            [str(x) for x in (resolved.get("resolved_dataset_names") or []) if str(x)],
        )

    _scope_files_loading = {"v": False}
    _scope_files_collapsed = {"v": True}

    def _toggle_scope_files() -> None:
        _scope_files_collapsed["v"] = not _scope_files_collapsed["v"]
        asyncio.create_task(_refresh_scope_files_panel())
    _scope_layer_labels = {
        "text": "текст",
        "graphics": "графика",
        "tables": "таблицы",
        "calculations": "расчёты",
        "technical_docs": "техничка",
        "drawings": "чертежи",
        "cad_bim": "BIM",
        "normative": "нормы",
        "estimate": "сметы",
    }

    async def _ask_about_scope_file(file_name: str) -> None:
        target = str(file_name or "").strip()
        if not target:
            return
        _pending_target_file["v"] = target
        chat_input.value = f"расскажи, что в файле {target}"
        _update_prompt_preview()
        await send_chat()

    def _scope_file_label(card: dict) -> str:
        file_name = str(card.get("file_name") or "")
        return file_name.rsplit("/", 1)[-1] or file_name or "файл"

    def _scope_file_badges(card: dict) -> list[str]:
        layers = card.get("content_layer_labels") or card.get("content_layers") or []
        labels = [_scope_layer_labels.get(str(layer), str(layer)) for layer in layers if str(layer)]
        role = str(card.get("document_role") or "").strip()
        if role and role not in labels:
            labels.insert(0, role)
        return labels[:4]

    async def _refresh_scope_files_panel() -> None:
        if _scope_files_loading["v"]:
            return
        ds_ids, ds_names = await _resolve_scope_dataset_ids()
        if not ds_ids:
            scope_files_panel.clear()
            scope_files_panel.set_visibility(False)
            return
        _scope_files_loading["v"] = True
        scope_files_panel.set_visibility(True)
        scope_files_panel.clear()
        with scope_files_panel:
            with ui.row().classes("sov-scope-files-head"):
                ui.icon("o_folder_open").classes("sov-scope-files-icon")
                ui.label("Файлы выбранной области").classes("sov-scope-files-title")
                ui.label("загружаю...").classes("sov-scope-files-note")
        try:
            memories: list[dict] = []
            from urllib.parse import quote as _q
            for dsid in ds_ids[:3]:
                memory = await api_get(f"/api/notebooks/{_q(dsid, safe='')}/memory")
                memory = dict(memory) if isinstance(memory, dict) else {}
                if not memory.get("file_cards"):
                    registry = await api_get(
                        f"/api/rag/documents?dataset_id={_q(dsid, safe='')}&limit=100"
                    )
                    if isinstance(registry, dict):
                        memory["file_cards"] = list(registry.get("documents") or [])
                memory.setdefault("dataset_id", dsid)
                memories.append(memory)
            scope_files_panel.clear()
            with scope_files_panel:
                with ui.row().classes("sov-scope-files-head"):
                    ui.icon("o_folder_open").classes("sov-scope-files-icon")
                    title = "Файлы датасета" if len(ds_ids) == 1 else "Файлы выбранной области"
                    ui.label(title).classes("sov-scope-files-title")
                    total_files = sum(len(m.get("file_cards") or []) for m in memories)
                    ui.label(f"{len(ds_ids)} датасет(ов) · {total_files} файлов").classes("sov-scope-files-note")
                    ui.button(
                        icon="o_expand_more" if _scope_files_collapsed["v"] else "o_expand_less",
                        on_click=_toggle_scope_files,
                    ).props('flat round dense aria-label="Показать или скрыть файлы"').classes("sov-icon-btn")
                    ui.button(
                        icon="o_refresh",
                        on_click=lambda: asyncio.create_task(_refresh_scope_files_panel()),
                    ).props('flat round dense aria-label="Обновить файлы датасета"').classes("sov-icon-btn")
                shown = 0
                with ui.row().classes("sov-scope-files-list") as scope_files_list:
                    for idx, memory in enumerate(memories):
                        dataset_name = (
                            (ds_names[idx] if idx < len(ds_names) else "")
                            or memory.get("dataset_name")
                            or memory.get("dataset_id")
                            or "датасет"
                        )
                        cards = list(memory.get("file_cards") or [])
                        cards.sort(
                            key=lambda card: (
                                0 if str(card.get("document_role") or "") else 1,
                                _scope_file_label(card).lower(),
                            )
                        )
                        for card in cards[:18]:
                            file_name = str(card.get("file_name") or "")
                            if not file_name:
                                continue
                            base_name = file_name.rsplit("/", 1)[-1]
                            if base_name.startswith(".") or base_name.startswith("_les_"):
                                continue
                            shown += 1
                            with ui.element("div").classes("sov-scope-file-chip"):
                                ui.label(_scope_file_label(card)).classes("sov-scope-file-name")
                                if len(ds_ids) > 1:
                                    ui.label(str(dataset_name)[:38]).classes("sov-scope-file-dataset")
                                elif "/" in file_name:
                                    ui.label(file_name.rsplit("/", 1)[0][-48:]).classes("sov-scope-file-dataset")
                                badges = _scope_file_badges(card)
                                if badges:
                                    with ui.row().classes("sov-scope-file-badges"):
                                        for badge in badges:
                                            ui.label(str(badge)).classes("sov-scope-file-badge")
                                ui.button(
                                    icon="o_chat",
                                    on_click=lambda f=file_name: asyncio.create_task(_ask_about_scope_file(f)),
                                ).props('flat round dense aria-label="Спросить по файлу"').classes(
                                    "sov-scope-file-ask"
                                ).tooltip(f"Спросить строго по файлу: {file_name}")
                scope_files_list.set_visibility(not _scope_files_collapsed["v"])
                if not shown:
                    no_files_label = ui.label("В выбранной области пока нет карточек файлов.").classes("sov-muted")
                    no_files_label.set_visibility(not _scope_files_collapsed["v"])
                if len(ds_ids) > 3:
                    overflow_label = ui.label(f"Показаны первые 3 датасета из {len(ds_ids)}. Остальные остаются в области поиска.").classes("sov-muted")
                    overflow_label.set_visibility(not _scope_files_collapsed["v"])
        finally:
            _scope_files_loading["v"] = False

    asyncio.create_task(_refresh_scope_files_panel())


    # W18.1: файл-вьювер (дерево RAG_Content + просмотр текст/код/картинка/PDF).

    # Состояние файл-дерева: карта «папка ли» + множество уже-дозагруженных папок
    # + ссылка на сам ui.tree (для ленивой дозагрузки поддеревьев по раскрытию).
    _file_state: dict = {"is_dir": {}, "loaded": set(), "tree": None}
    _LAZY = "\x00lazy"  # маркер заглушки-ребёнка нераскрытой папки


    # W17.5: КАРТА ОБЪЕКТА — паспорт объекта (досье), собирается из /api/projects/{id}/dossier (0 LLM).

    async def _load_sessions():
        sessions_col.clear()
        pid = workspace.active.get("project_id")
        suffix = f"?project_id={pid}" if pid else ""
        result = await api_get(f"/api/workspace/sessions{suffix}")
        data = result.get("sessions", []) if isinstance(result, dict) else list(result or [])
        if not data:
            with sessions_col:
                _html('<div class="sov-muted" style="padding:14px;">Нет сохранённых сессий</div>')
            return
        with sessions_col:
            for session in data:
                _render_session_card(session)

    def _render_session_card(session: dict):
        sid = session["session_id"]
        first_q = session.get("title") or session.get("first_question") or "Новый чат"
        started_at = (session.get("created_at") or session.get("started_at") or "")[:16].replace("T", " ")
        with ui.element("button").classes("sov-session-card") as card:
            _html(f'<span class="sov-session-title">{esc(first_q[:90])}</span>')
            _html(f'<span class="sov-session-meta">{esc(started_at)}</span>')

        async def _open(session_id=sid, el=card):
            await _open_session(session_id, el)

        card.on("click", _open)

    async def _open_session(session_id: str, el=None):
        await workspace.run(lambda: workspace.open_session(session_id))

    async def _activate_workspace_session(record: dict) -> bool:
        from sovushka.state import persist_session_id
        session_id = record["session_id"]
        add_log(f"[ИСТОРИЯ] Загружаю сессию {session_id[:8]}...")
        msgs = await api_get(f"/api/chat/history?session_id={session_id}")
        if msgs is None:
            add_log("[ИСТОРИЯ] Ошибка загрузки сессии")
            return False
        failure_notice.clear()
        _clear_attachment(notify=False)
        artifacts._clear_file_artifacts()
        artifact_panel.clear()
        with artifact_panel:
            _render_empty_artifacts()
        detail_dataset.set_value("(все датасеты)")
        chat_input.set_value(drafts.switch(session_id, chat_input.value))
        _pending_target_files["v"] = []
        _pending_target_file["v"] = ""
        state["chat_pending"] = None
        reranker_checkbox.set_value(False)
        scope_state.clear()
        scope_state.update(record.get("scope") or {
            "scope_type": "none", "project_ids": [], "dataset_ids": [],
            "selected_sources_only": False,
        })
        project_state["id"] = record.get("project_id")
        scope_state["label"] = _scope_label()
        scope_btn.set_text(scope_state["label"])
        selected_sources_only_switch.set_value(bool(scope_state.get("selected_sources_only")))
        role = record.get("role", default_chat_mode())
        mode_select.set_value(role if role in visible_chat_modes() else default_chat_mode())
        state["chat_history"] = msgs
        state["load_session_id"] = None
        state["session_id"] = persist_session_id(session_id)
        if selected_session_card["el"]:
            selected_session_card["el"].classes(remove="sov-session-card-active")
        _render_chat_history("Продолжение чата." if msgs else "Новый чат. Выберите источники или задайте вопрос.")
        scope_opts_cache["data"] = None
        history_drawer.set_visibility(False)
        _scroll_chat_to_tail(force=True)
        await _refresh_scope_files_panel()
        return True


    from sovushka.components.source_panels import build_source_panels
    _show_source_drawer, _show_sources_artifact = build_source_panels(artifact_panel, lambda: artifacts._open_artifacts(), _copy_button)


    messages = ChatMessages(
        chat_column=chat_column,
        chat_input=chat_input,
        detail_dataset=detail_dataset,
        empty_state_ref=empty_state_ref,
        _link_visible_sources=lambda *args, **kwargs: _link_visible_sources(*args, **kwargs),
        _render_rich_body=lambda *args, **kwargs: _render_rich_body(*args, **kwargs),
        _show_source_drawer=lambda *args, **kwargs: _show_source_drawer(*args, **kwargs),
        _show_sources_artifact=lambda *args, **kwargs: _show_sources_artifact(*args, **kwargs),
        _source_anchor_prefix=lambda *args, **kwargs: _source_anchor_prefix(*args, **kwargs),
        _source_markdown=lambda *args, **kwargs: _source_markdown(*args, **kwargs),
        _update_prompt_preview=lambda *args, **kwargs: _update_prompt_preview(*args, **kwargs),
        send_chat=lambda *args, **kwargs: send_chat(*args, **kwargs),
        get_artifacts=lambda: artifacts,
    )
    def _finish_ai_placeholder(bubble, label, text, srcs=None, crag="", error=False, meta=None):
        messages._finish_ai_placeholder(bubble, label, text, srcs, crag, error, meta)
        if error or crag == "BLOCKED":
            failure_notice.show(bubble)
    _render_chat_bubble = messages._render_chat_bubble
    _render_chat_history = messages._render_chat_history
    _render_msg = messages._render_msg
    _render_source_tags = messages._render_source_tags
    _render_suggestions = messages._render_suggestions


    async def _ask_about_inventory_file(file_name: str) -> None:
        target = str(file_name or "").strip()
        if not target:
            return
        _pending_target_file["v"] = target
        chat_input.value = f"расскажи, что в файле {target}"
        _update_prompt_preview()
        await send_chat()

    async def _ask_about_inventory_status(status: str) -> None:
        value = str(status or "").strip().upper()
        if not value:
            return
        chat_input.value = f"расскажи про файлы со статусом {value}"
        _update_prompt_preview()
        await send_chat()

    async def _restudy_inventory_dataset() -> None:
        chat_input.value = "изучи датасет заново: дай карту файлов, слои данных и что в каких документах искать"
        _update_prompt_preview()
        await send_chat()


    from sovushka.components.chat_answer_body import _source_anchor_prefix, _link_visible_sources, _render_rich_body, _source_markdown, _format_sources_as_quotes


    async def _refresh_active_model_chip() -> None:
        data = await api_get("/api/status") or {}
        proxy = data.get("proxy") if isinstance(data.get("proxy"), dict) else {}
        llm_provider = proxy.get("llm_provider") if isinstance(proxy.get("llm_provider"), dict) else {}
        label = _model_label(llm_provider.get("provider", ""), llm_provider.get("model", ""))
        if label:
            model_chip.set_text(f"МОДЕЛЬ {label}")
        else:
            model_chip.set_text("МОДЕЛЬ —")


    def _apply_loaded_session() -> bool:
        """Подхват сессии, выбранной в ИСТОРИИ. Вызывается и при построении,
        и хуком из вкладки истории (фикс «чат из истории не открывается»)."""
        if not state.get("load_session_id"):
            return False
        from sovushka.state import persist_session_id

        state["session_id"] = state["load_session_id"]
        state["load_session_id"] = None
        persist_session_id(state["session_id"])
        asyncio.create_task(_open_session(state["session_id"]))
        return True

    # Хук для вкладки ИСТОРИЯ: после выбора сессии чат перерисовывается сразу,
    # а не «при следующем рендере» (которого без хука не наступало).
    state["chat_reload_hook"] = _apply_loaded_session

    async def _load_history():
        from sovushka.state import ensure_session_id, persist_session_id

        if _apply_loaded_session():
            return
        sid = ensure_session_id()
        hist = await api_get(f"/api/chat/history?session_id={sid}")
        if hist is None:
            return
        state["chat_history"] = list(hist or [])
        persist_session_id(sid)
        if state["chat_history"]:
            _render_chat_history("Сессия восстановлена.")
            _scroll_chat_to_tail(force=True)

    async def _restore_workspace_history():
        workspace.navigating = True
        try:
            await _load_history()
        finally:
            workspace.navigating = False
        await workspace.run(workspace.restore)
        await project_navigation.refresh()

    asyncio.create_task(_restore_workspace_history())

    async def load_output_template(e):
        content = e.content.read()
        fname = e.name
        try:
            if fname.endswith(".json"):
                data = json.loads(content.decode("utf-8"))
                state["output_template"] = data if isinstance(data, list) else [data]
            elif fname.endswith(".csv"):
                lines = content.decode("utf-8").strip().split("\n")
                keys = [k.strip() for k in lines[0].split(",")]
                rows = [dict(zip(keys, [v.strip() for v in row.split(",")])) for row in lines[1:] if row.strip()]
                state["output_template"] = rows
            elif fname.endswith(".xlsx"):
                import tempfile
                import openpyxl

                with tempfile.NamedTemporaryFile(suffix=".xlsx", delete=False) as tf:
                    tf.write(content)
                    tf.flush()
                    wb = openpyxl.load_workbook(tf.name)
                    ws = wb.active
                    headers = [str(c.value or "").strip() for c in next(ws.iter_rows(max_row=1))]
                    rows = []
                    for row in list(ws.iter_rows(min_row=2, values_only=True))[:20]:
                        rows.append(dict(zip(headers, [str(v or "") for v in row])))
                    state["output_template"] = rows
            else:
                ui.notify("Поддерживаются JSON, CSV, XLSX", type="warning")
                return

            tmpl = state["output_template"]
            template_lbl.set_text(f"{fname} · {len(tmpl)} строк")
            if tmpl:
                preview_str = json.dumps(tmpl[0], ensure_ascii=False, indent=2)
                template_preview.set_content(f'<pre>{esc(preview_str)}</pre>')
            add_log(f"[ШАБЛОН] Загружен {fname} · {len(tmpl)} строк")
            _update_prompt_preview()
        except Exception as ex:
            ui.notify(f"Ошибка парсинга: {ex}", type="negative")
            add_log(f"[ШАБЛОН] Ошибка: {ex}")

    def _build_extra_prompt(question: str) -> str:
        mode = out_mode_val["v"]
        style_map = {
            "Русский (технический)": "Пиши профессиональным техническим языком.",
            "Русский (нормативный ГОСТ)": "Пиши в нормативном стиле ГОСТ: чёткие формулировки, без лирики.",
            "Краткие тезисы": "Отвечай тезисами — каждый пункт одна мысль.",
            "Для презентации": "Формат для слайдов: заголовок + маркированный список.",
        }

        parts = []
        if style_map.get(detail_lang.value):
            parts.append(style_map[detail_lang.value])

        if mode == "spec":
            gost_str = " строго по форме ГОСТ 21.110-2013" if spec_gost.value else ""
            group_str = " Группируй по разделам." if spec_group.value else ""
            parts.append(
                f"\n\nВЫВЕДИ ОТВЕТ В ФОРМАТЕ СПЕЦИФИКАЦИИ{gost_str}.\n"
                f"Тип: {spec_type.value}.{group_str}\n"
                "Верни JSON-массив объектов. Обязательные поля: "
                "поз, обозначение, наименование, тип_марка, ед_изм, кол_во, масса_ед, примечание.\n"
                "Оберни в ```json ... ```"
            )
        elif mode == "schema":
            depth = int(schema_depth.value) if schema_depth.value else 3
            fmt = schema_format.value
            if fmt == "JSON дерево":
                parts.append(
                    f"\n\nВЫВЕДИ ОТВЕТ В ВИДЕ JSON-ДЕРЕВА, глубина {depth}. "
                    "Структура узла: {\"name\": str, \"children\": [...], \"desc\": str}. "
                    "Оберни в ```json ... ```"
                )
            elif fmt == "YAML":
                parts.append(f"\n\nВЫВЕДИ ОТВЕТ В ВИДЕ YAML-ДЕРЕВА, глубина {depth}. Оберни в ```yaml ... ```")
            else:
                parts.append(f"\n\nВЫВЕДИ ОТВЕТ В ВИДЕ {fmt.upper()}, глубина {depth} уровней.")
        elif mode == "structure":
            parts.append("\n\nВЫВЕДИ ОТВЕТ В ВИДЕ СТРУКТУРИРОВАННОГО JSON-ОБЪЕКТА. Оберни в ```json ... ```")
        elif mode == "table":
            parts.append("\n\nВЫВЕДИ ОТВЕТ В ВИДЕ ТАБЛИЦЫ: JSON-массив объектов. Оберни в ```json ... ```")
        elif mode == "mermaid":
            parts.append(
                f"\n\nВЫВЕДИ ОТВЕТ В ВИДЕ MERMAID-ДИАГРАММЫ типа {mermaid_type.value}. "
                "Оберни в ```mermaid ... ```. Пиши на русском, метки узлов короткие."
            )
        elif mode == "svg":
            w, h = svg_size.value.split("×") if "×" in svg_size.value else ("800", "600")
            parts.append(
                f"\n\nВЫВЕДИ ОТВЕТ В ВИДЕ SVG-СХЕМЫ ({svg_type.value}). "
                f"Размер viewBox: 0 0 {w} {h}. Оберни в ```svg ... ```"
            )
        elif mode == "template":
            tmpl = state.get("output_template")
            if tmpl:
                parts.append(
                    "\n\nОТВЕЧАЙ СТРОГО ПО СТРУКТУРЕ ОБРАЗЦА (JSON-массив).\n"
                    f"Образец:\n```json\n{json.dumps(tmpl[:3], ensure_ascii=False, indent=2)}\n```\n"
                    "Оберни в ```json ... ```"
                )
            else:
                parts.append("\n\nОТВЕЧАЙ В ВИДЕ JSON-МАССИВА ОБЪЕКТОВ. Оберни в ```json ... ```")

        if detail_extra.value and detail_extra.value.strip():
            parts.append(f"\n\nДОПОЛНИТЕЛЬНО: {detail_extra.value.strip()}")
        return " ".join(parts[:2]) + "".join(parts[2:])

    def _update_prompt_preview():
        q = chat_input.value.strip() or "[текст запроса]"
        extra = _build_extra_prompt(q)
        preview_text = (q + extra)[:1000] + ("..." if len(q + extra) > 1000 else "")
        prompt_preview.set_content(f'<pre>{esc(preview_text)}</pre>')

    chat_input.on("input", lambda: _update_prompt_preview())

    async def _clear_chat():
        if await workspace.run(workspace.new_session):
            reranker_checkbox.set_value(False)
            await _refresh_active_model_chip()

    _sending = {"v": False}
    _active_send_task: dict[str, asyncio.Task | None] = {"task": None}
    _resource_blocked = {"v": False, "reason": ""}

    def _indexing_summary(data: dict) -> str:
        rag = state.get("rag_health", {}) if isinstance(state.get("rag_health"), dict) else {}
        totals = rag.get("totals", {}) if isinstance(rag, dict) else {}
        indexed = totals.get("indexed_files", "—")
        pending = totals.get("pending_files", "—")
        errors = totals.get("error_files", "—")
        chunks = totals.get("chunks", "—")
        reason = _runtime_guard_reason_label(
            data.get("chat_generation_reason") or "Индексация активна."
        )
        if not data.get("active"):
            return f"Чат временно заблокирован защитой ресурсов. {reason}."
        return (
            f"Индексация активна: чат заблокирован. "
            f"Проиндексировано: {indexed} · ожидают: {pending} · ошибок: {errors} · фрагментов: {chunks}. "
            f"{reason}"
        )

    def _set_chat_blocked(blocked: bool, reason: str = ""):
        _resource_blocked["v"] = blocked
        _resource_blocked["reason"] = reason
        if blocked:
            response_settings_btn.props("disabled")
            if not _sending["v"]:
                send_btn.props(remove="disabled")
                apply_btn.props(remove="disabled")
                chat_input.props(remove="disabled")
            composer_box.classes(add="sov-composer-blocked")
            indexing_banner.set_text(reason)
            indexing_banner.set_visibility(True)
            mode_chip.set_text("PAUSED")
            mode_chip.classes(add="sov-chip-soft")
            return
        if not _sending["v"]:
            send_btn.props(remove="disabled")
            apply_btn.props(remove="disabled")
            response_settings_btn.props(remove="disabled")
            chat_input.props(remove="disabled")
        composer_box.classes(remove="sov-composer-blocked")
        indexing_banner.set_visibility(False)
        mode_chip.set_text("RAG")
        mode_chip.classes(remove="sov-chip-soft")

    async def _refresh_resource_gate() -> bool:
        data = await refresh_indexing_mode()
        allowed = bool(data.get("chat_generation_allowed", True)) if isinstance(data, dict) else True
        blocked = not allowed
        _set_chat_blocked(blocked, _indexing_summary(data) if blocked else "")
        return allowed

    def _stop_active_dialog() -> None:
        task = _active_send_task["task"]
        if not _sending["v"] or task is None or task.done():
            ui.notify("Сейчас нет активного ответа", type="info")
            return
        add_log("[ЧАТ] Пользователь остановил текущий ответ")
        task.cancel()
        ui.notify("Останавливаю текущий ответ…", type="info")


    async def _do_send(question: str):
        if _sending["v"]:
            return
        failure_notice.clear()
        _sending["v"] = True
        try:
            await workspace.persist(
                scope={key: scope_state.get(key) for key in (
                    "scope_type", "project_ids", "dataset_ids", "selected_sources_only"
                )}, role=out_mode_val["v"], title=question,
            )
        except RuntimeError as error:
            _sending["v"] = False
            ui.notify(str(error), type="negative")
            return
        try:
            empty_state_ref["el"].set_visibility(False)
        except Exception:
            pass
        selected_dataset_filter = (
            detail_dataset.value
            if detail_dataset.value and detail_dataset.value != "(все датасеты)"
            else None
        )
        skip_resource_gate = should_skip_chat_resource_gate(question, selected_dataset_filter)
        if skip_resource_gate:
            _set_chat_blocked(False)
        else:
            await _refresh_resource_gate()
        _sending["v"] = True
        _active_send_task["task"] = asyncio.current_task()
        send_btn.props("disabled")
        apply_btn.props("disabled")
        response_settings_btn.props("disabled")
        chat_input.props("disabled")
        stop_dialog_btn.set_visibility(True)
        sent_attachment = dict(attach_state) if attach_state.get("id") else {}
        # Авто-GOST: «собери/составь спецификацию …» → формат спеки (ГОСТ 21.110), чтобы
        # артефакт был чистой таблицей по форме, а не прозой. Не липко — селектор вернём.
        _orig_mode = out_mode_val["v"]
        if _orig_mode not in visible_chat_modes() and _is_spec_request(question):
            out_mode_val["v"] = "spec"
        out_mode = out_mode_val["v"]
        # МАРШРУТНЫЕ РЕЖИМЫ: backend форсит путь по полю mode (минуя угадайку роутера).
        #   smeta → model+RAG сметчик; review → таблица проверки; kp → текст-задел; rag → заземлённый RAG;
        #   free → вольный LLM без ретрива. Прочие out_mode (table/svg/…) — обычный формат-режим.
        _ROUTING_MODES = {
            "search", "agent", "engineer",
            "review", "kp", "rag", "free", "doc_review",
        }
        _routing_mode = out_mode if out_mode in _ROUTING_MODES else None
        _ph_map = {"engineer": "Проверяю проект…", "kp": "Готовлю КП…",
                   "agent": "Исследую источники и файлы…",
                   "free": "Думаю вольно…", "search": "Ищу в документах…", "doc_review": "Проверяю по ГОСТ…"}
        _initial_status = _ph_map.get(out_mode, "Генерирую…")
        # рендер ответа: смета теперь проза model+RAG; проверка → таблица; вольный/раг/кп → текст.
        _render_mode = "table" if out_mode == "review" else ("text" if _routing_mode else out_mode)
        payload_question, pasted_context, pasted_suffix = _split_pasted_context_for_payload(
            question,
            mode=out_mode,
            has_attachment=bool(sent_attachment),
        )
        attachment_suffix = _attachment_user_suffix(sent_attachment)
        question_display = payload_question if pasted_suffix else question
        display_notes = [x for x in (attachment_suffix, pasted_suffix) if x]
        if display_notes:
            question_display = f"{question_display}\n\n" + "\n".join(display_notes)

        request_started_at = datetime.now().astimezone().isoformat(timespec="seconds")
        state["chat_history"].append({
            "role": "user",
            "text": question_display,
            "requested_at": request_started_at,
        })
        state["chat_pending"] = {
            "question": payload_question,
            "started_at": time.time(),
            "requested_at": request_started_at,
        }

        with chat_column:
            _render_chat_bubble(
                question_display,
                "chat-msg-user",
                meta={"requested_at": request_started_at},
            )
            ai_placeholder, ai_placeholder_label = _render_ai_placeholder(f"{_initial_status} 0с")
            activity = ActivityPanel(_initial_status)

        _scroll_chat_to_tail(force=True)
        add_log(f'[AI] Запрос: "{payload_question[:60]}"')

        # Формат/стиль ответа — ОТДЕЛЬНЫМ полем (не клеим в текст вопроса): иначе бэкенд
        # видит мусор-шаблон как вопрос → авто-заметки/роутинг/ретрив плывут.
        # В маршрутном режиме формат-директива не нужна (backend сам решает, что вернуть).
        extra_prompt = "" if (skip_resource_gate or _routing_mode) else _build_extra_prompt(payload_question)
        out_mode_val["v"] = _orig_mode  # вернуть пользователю его выбор формата (авто-GOST не липкий)
        payload = {
            "question": payload_question,
            "output_directive": extra_prompt or None,
            "session_id": state.get("session_id"),
            "response_length": str(response_length_select.value or "standard"),
            "reranker_enabled": bool(reranker_checkbox.value),
        }
        payload["selected_sources_only"] = bool(
            scope_state.get("selected_sources_only", False)
        )
        if apply_active_profile["v"]:
            payload["apply_profile_revision"] = True
            apply_active_profile["v"] = False
        target_files = [str(x).strip() for x in (_pending_target_files.get("v") or []) if str(x).strip()]
        if target_files:
            payload["target_files"] = target_files
            _pending_target_files["v"] = []
        target_file = str(_pending_target_file.get("v") or "").strip()
        if target_file:
            payload["target_file"] = target_file
            _pending_target_file["v"] = ""
        if _routing_mode:
            payload["mode"] = _routing_mode
            out_mode = _render_mode  # дальше ответ рендерится в выбранном виде (таблица/текст)
        if detail_dataset.value and detail_dataset.value != "(все датасеты)":
            payload["dataset_filter"] = detail_dataset.value
        # v0.22: явная ОБЛАСТЬ ПОИСКА из ScopeSelector (приоритетнее legacy; backend сам резолвит).
        payload["scope"] = {"scope_type": scope_state["scope_type"],
                            "project_ids": scope_state["project_ids"],
                            "dataset_ids": scope_state["dataset_ids"]}
        payload["project_id"] = workspace.active.get("project_id")
        payload.update(_attachment_chat_payload(sent_attachment))
        if pasted_context:
            existing_ctx = str(payload.get("attachment_context") or "").strip()
            payload["attachment_context"] = (
                f"{existing_ctx}\n\n{pasted_context}" if existing_ctx else pasted_context
            )[:20000]
        _t0 = time.monotonic()
        _stop_tick = {"v": False}
        _status_text = {"v": _initial_status}
        stream_state = {"text": "", "got_token": False, "got_progress": False, "final": None, "error": None}

        async def _tick():
            while not _stop_tick["v"]:
                elapsed = int(time.monotonic() - _t0)
                activity.tick()
                if not stream_state["text"] and not activity.finished:
                    ai_placeholder_label.set_text(f"{_status_text['v']} {elapsed}с")
                await asyncio.sleep(1)

        _tick_task = asyncio.create_task(_tick())

        async def _gen_form_from_command(cmd: dict) -> None:
            """Создать документ по /-команде: генерация формы → карточка в панели «Файлы»
            (предпросмотр + скачивание кнопками), а не принудительный диалог сохранения."""
            fid = cmd.get("form_id")
            fmt = cmd.get("fmt", "xlsx")
            if cmd.get("download"):
                nm = (
                    cmd.get("filename")
                    or (Path(str(cmd.get("path") or "")).name if cmd.get("path") else "")
                    or f"{fid}.{fmt}"
                )
                if "." not in str(nm).rsplit("/", 1)[-1]:
                    nm = f"{nm}.{fmt}"
                artifacts._register_file_artifact(nm, cmd["download"], _kind_from_name(nm))
                ui.notify(
                    f"Документ создан: {cmd.get('title', fid)} ({fmt}) — в панели «Файлы»",
                    type="positive",
                )
                return
            body: dict = {"fmt": fmt}
            if cmd.get("source"):
                body["source"] = cmd["source"]
            if cmd.get("project_id") is not None:
                body["project_id"] = cmd["project_id"]
            sid = state.get("session_id") or ""
            if sid:
                body["session_id"] = sid
            gen = await api_post(f"/api/forms/{fid}/generate", body)
            if not isinstance(gen, dict) or not gen.get("download"):
                ui.notify(last_api_error_text("Не удалось создать документ"), type="negative")
                return
            nm = (
                gen.get("filename")
                or gen.get("path", "").rsplit("/", 1)[-1]
                or f"{fid}.{fmt}"
            )
            if "." not in str(nm).rsplit("/", 1)[-1]:
                nm = f"{nm}.{fmt}"
            artifacts._register_file_artifact(nm, gen["download"], _kind_from_name(nm))
            ui.notify(f"Документ создан: {cmd.get('title', fid)} ({fmt}) — в панели «Файлы»", type="positive")


        succeeded = False

        def _apply_chat_result(d: dict) -> None:
            nonlocal succeeded
            """Применяет финальный payload (общий для стрима и нестриминга):
            форматированный ответ, источники, вердикт, артефакт."""
            if sent_attachment and not _preserved_attachment(d, sent_attachment):
                _clear_attachment(notify=False)
            ans = d.get("answer", d.get("response", "Нет ответа"))
            srcs = d.get("sources", [])
            crag = d.get("crag_status", "")
            retrieval_trace = d.get("retrieval_trace") or {}
            blocker = d.get("blocker") or {}
            total_status = d.get("total_status") or ""
            if not total_status and (
                str(crag).upper() == "BLOCKED"
                or retrieval_trace.get("status") == "blocked"
            ):
                total_status = "blocked"
            succeeded = total_status != "blocked" and not d.get("partial")
            activity.finish("Готово" if succeeded else "Запрос не выполнен")
            meta = {
                "query_route": d.get("query_route") or {},
                "retrieval_trace": retrieval_trace,
                "unified_trace": d.get("unified_trace") or retrieval_trace,
                "evidence_summary": d.get("evidence_summary") or {},
                "total_status": total_status,
                "blocker": blocker,
                "cache": d.get("cache", "miss"),
                "validation": d.get("validation") or {"enabled": True},
                "history_id": d.get("history_id"),
                "table_query": d.get("table_query"),
                "latency_phases": d.get("latency_phases") or (d.get("retrieval_trace") or {}).get("latency_phases"),
                "requested_at": request_started_at,
                "elapsed_sec": round(max(0.0, time.monotonic() - _t0), 1),
                "out_mode": out_mode,
                "clarifying_questions": d.get("clarifying_questions") or [],
                "suggested_filters": d.get("suggested_filters") or [],
                "class_suggestions": d.get("class_suggestions") or [],
                "source_excerpts": d.get("source_excerpts") or [],
                "scenario": d.get("scenario") or {},
                "answer_contract": d.get("answer_contract") or {},
                "answer_contract_check": d.get("answer_contract_check") or {},
                "workflow_plan": d.get("workflow_plan") or {},
                "artifact": d.get("artifact") or {},
                "source_map": d.get("source_map") or {},
                "source_counts": d.get("source_counts") or {},
                "project_inventory": d.get("project_inventory") or {},
                "versions": d.get("versions") or {},
            }
            state["chat_history"].append({"role": "ai", "text": ans, "srcs": srcs, "crag": crag, "meta": meta})
            _finish_ai_placeholder(ai_placeholder, ai_placeholder_label, ans, srcs, crag, meta=meta)
            artifacts._register_artifact_downloads(meta)
            explicit_artifact = _artifact_from_meta(meta)
            if explicit_artifact and (artifact_shell.visible or _inventory_file_rows_from_meta(meta)):
                artifacts._show_meta_artifact(meta, str(explicit_artifact.get("content") or ""), str(explicit_artifact.get("mode") or "text"), srcs)
            # Режим «Смета» выключен, но запрос похож на объектную смету → предложить
            # пересчитать капстоуном (детерминированный расчёт вместо RAG-осколков).
            _route_ch = (d.get("query_route") or {}).get("channel", "")
            add_log(f"[AI] Формат:{out_mode} CRAG:{crag or 'N/A'} src:{len(srcs)}")
            # W11.17: команда «создать документ» → генерируем форму и скачиваем файл.
            cmd = d.get("command") or {}
            if cmd.get("action") in {"generate_form", "generate_filled_form"} and cmd.get("form_id"):
                asyncio.create_task(_gen_form_from_command(cmd))
            # (панель «Задачи и объёмы» убрана из чата)

        # W5.1: SSE-стрим — токены в пузырь по мере генерации; финальное событие
        # несёт авторитетный payload (вердикт валидации в crag_status).
        early_sources = {"el": None}

        def _on_sse(event: str, payload) -> None:
            if event == "token":
                if not stream_state["got_token"]:
                    stream_state["got_token"] = True
                    activity.update({"label": "Получаю ответ модели"})
                stream_state["text"] += payload if isinstance(payload, str) else ""
                ai_placeholder_label.set_text(stream_state["text"])
                ai_placeholder_label.update()
                _scroll_chat_to_tail()
            elif event == "reset":
                stream_state["text"] = ""
                stream_state["got_token"] = False
                ai_placeholder_label.set_text("")
                ai_placeholder_label.update()
            elif event == "progress":
                stream_state["got_progress"] = True
                if isinstance(payload, dict):
                    activity.update(payload)
                    label = str(payload.get("label") or "Работаю...")
                    _status_text["v"] = label
                else:
                    _status_text["v"] = str(payload or "Работаю...")
                elapsed = int(time.monotonic() - _t0)
                if not stream_state["text"]:
                    ai_placeholder_label.set_text(f"{_status_text['v']} {elapsed}с")
                    ai_placeholder_label.update()
                _scroll_chat_to_tail()
            elif event == "tool_progress":
                stream_state["got_progress"] = True
                if isinstance(payload, dict):
                    activity.update(payload)
                    label = str(payload.get("label") or "Собираю файл").strip()
                    completed_rows = payload.get("completed")
                    total_rows = payload.get("total")
                    if completed_rows is not None and total_rows is not None:
                        label = f"{label} · {completed_rows}/{total_rows}"
                    _status_text["v"] = label
                else:
                    _status_text["v"] = str(payload or "Собираю файл")
                elapsed = int(time.monotonic() - _t0)
                if not stream_state["text"]:
                    ai_placeholder_label.set_text(f"{_status_text['v']} {elapsed}с")
                    ai_placeholder_label.update()
                _scroll_chat_to_tail()
            elif event == "sources":
                if isinstance(payload, dict) and not early_sources["el"]:
                    srcs = payload.get("sources") or []
                    meta = {
                        "source_excerpts": payload.get("source_excerpts") or [],
                        "source_map": payload.get("source_map") or [],
                    }
                    if srcs or meta["source_excerpts"]:
                        with ai_placeholder:
                            with ui.column().classes("sov-early-sources w-full gap-1 mt-2") as holder:
                                _render_source_tags(srcs, "", meta)
                        early_sources["el"] = holder
                        ai_placeholder.update()
                        _scroll_chat_to_tail()
            elif event == "final":
                stream_state["final"] = payload if isinstance(payload, dict) else {}
                result = stream_state["final"]
                activity.finish("Ответ оборвался — сохранён фрагмент" if result.get("partial") else
                                "Запрос не выполнен" if result.get("crag_status") == "BLOCKED" else "Готово")
            elif event == "error":
                stream_state["error"] = payload if isinstance(payload, dict) else {"detail": str(payload)}
                activity.finish("Не удалось завершить запрос")

        completed = False
        try:
            await api_post_stream("/api/chat/stream", payload, _on_sse)
            _stop_tick["v"] = True
            d = stream_state["final"]
            if d:
                completed = True
                if early_sources["el"]:
                    early_sources["el"].set_visibility(False)
                _apply_chat_result(d)
            elif stream_state["got_token"]:
                # Токены пришли, но финал потерян (обрыв середины стрима) —
                # не перегенерируем (дорого), показываем честную ошибку.
                completed = True
                err = stream_state["error"] or {}
                message = (
                    f"{err.get('status', '')}: {err.get('detail', '')}".strip(": ")
                    or last_api_error_text("Соединение прервано — ответ получен не полностью")
                )
                if err.get("status") == 409:
                    await _refresh_resource_gate()
                activity.finish("Ответ оборвался — получен не полностью")
                _finish_ai_placeholder(ai_placeholder, ai_placeholder_label,
                                       stream_state["text"] + "\n\n" + message, error=True)
                if artifact_shell.visible:
                    artifacts._render_artifact_error(message)
            else:
                serr = stream_state["error"] or {}
                if serr:
                    # SSE already carried the backend error. Do not start a second long /api/chat
                    # request: show the failure and stop the timer.
                    completed = True
                    activity.finish("Не удалось завершить запрос")
                    message = last_api_error_text(serr.get("detail") or "Ошибка запроса")
                    if serr.get("status") == 409:
                        await _refresh_resource_gate()
                    _finish_ai_placeholder(ai_placeholder, ai_placeholder_label, message, error=True)
                    if artifact_shell.visible:
                        artifacts._render_artifact_error(message)
                elif should_retry_unstreamed_chat(
                    got_token=bool(stream_state["got_token"]),
                    got_progress=bool(stream_state["got_progress"]),
                    stream_error=stream_state["error"],
                ):
                    # Ни одного токена и нет SSE error (стрим-эндпоинт недоступен/обрыв до событий) —
                    # безопасный откат на нестриминговый /api/chat.
                    d = await api_post("/api/chat", payload)
                    completed = True
                    if d:
                        _apply_chat_result(d)
                    else:
                        activity.finish("Не удалось завершить запрос")
                        err = state.get("last_api_error") or {}
                        message = last_api_error_text("Ошибка запроса")
                        if err.get("status_code") == 409:
                            await _refresh_resource_gate()
                        _finish_ai_placeholder(ai_placeholder, ai_placeholder_label, message, error=True)
                        if artifact_shell.visible:
                            artifacts._render_artifact_error(message)
                else:
                    completed = True
                    activity.finish("Соединение прервано")
                    message = "Соединение прервано. Повторите запрос для полного ответа."
                    _finish_ai_placeholder(ai_placeholder, ai_placeholder_label, message, error=True)
                    if artifact_shell.visible:
                        artifacts._render_artifact_error(message)
        except asyncio.CancelledError:
            activity.finish("Остановлено")
            completed = True
            _finish_ai_placeholder(
                ai_placeholder,
                ai_placeholder_label,
                (stream_state["text"] + "\n\n" if stream_state["text"] else "") + "Ответ остановлен пользователем и может быть неполным.",
                [],
                "",
                meta={"out_mode": out_mode},
            )
            add_log("[ЧАТ] Текущий ответ остановлен")
        except Exception as ex:
            activity.finish("Не удалось завершить запрос")
            completed = True
            add_log(f"[CHAT ERROR] {type(ex).__name__}: {ex}")
            message = "Не удалось завершить запрос. Повторите вопрос или откройте диагностику."
            _finish_ai_placeholder(ai_placeholder, ai_placeholder_label, message, error=True)
            if artifact_shell.visible:
                artifacts._render_artifact_error(message)
        finally:
            if completed:
                state["chat_pending"] = None
            if not completed:
                activity.finish("Запрос прерван")
            _stop_tick["v"] = True
            _tick_task.cancel()
            _sending["v"] = False
            _active_send_task["task"] = None
            stop_dialog_btn.set_visibility(False)
            await _refresh_resource_gate()
            _scroll_chat_to_tail()

        return succeeded

    async def send_chat():
        q = chat_input.value.strip()
        if not q:
            return
        chat_input.value = ""
        _update_prompt_preview()
        if not await _do_send(q):
            _restore_failed_question(chat_input, drafts, q)

    async def send_with_form():
        q = chat_input.value.strip()
        if not q:
            ui.notify("Введите текст запроса", type="warning")
            return
        advanced_dialog.close()
        chat_input.value = ""
        _update_prompt_preview()
        if not await _do_send(q):
            _restore_failed_question(chat_input, drafts, q)


    artifacts = ChatArtifacts(
        _ask_about_inventory_file=lambda *args: _ask_about_inventory_file(*args),
        _ask_about_inventory_status=lambda *args: _ask_about_inventory_status(*args),
        _restudy_inventory_dataset=lambda *args: _restudy_inventory_dataset(*args),
        artifact_divider=artifact_divider,
        artifact_panel=artifact_panel,
        artifact_shell=artifact_shell,
        chat_shell=chat_shell,
        files_artifacts_panel=files_artifacts_panel,
        tab_mermaid=tab_mermaid,
        tabs=tabs,
    )

    select_format("text")
    asyncio.create_task(_refresh_active_model_chip())
    asyncio.create_task(_refresh_resource_gate())
    resource_gate_timer = ui.timer(5.0, lambda: asyncio.create_task(_refresh_resource_gate()))
    model_chip_timer = ui.timer(30.0, lambda: asyncio.create_task(_refresh_active_model_chip()))
    context.client.on_disconnect(lambda *_: (resource_gate_timer.cancel(), model_chip_timer.cancel()))
    # .exact: отправка ТОЛЬКО на чистый Enter; Shift+Enter (и любой модификатор) → дефолтный
    # перенос строки в textarea (раньше .prevent убивал перенос на любом Enter).
    chat_input.on(
        "keydown.enter.exact.prevent",
        lambda e: asyncio.create_task(send_chat()) if not _resource_blocked["v"] else None,
    )
    return {"timers": [resource_gate_timer, model_chip_timer]}
