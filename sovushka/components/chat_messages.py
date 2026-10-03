"""Conversation messages, source chips and history rendering."""
from __future__ import annotations
from sovushka.components.chat_rendering import _artifact_from_meta, _bubble_text, _render_answer_actions, _render_answer_timing, _render_dataset_scope_badge, _render_evidence_header, _render_excerpts, _render_model_badge, _source_label
from sovushka.components.chat_presentation import format_chat_request_clock, _operator_status_chips, _operator_technical_chips
import asyncio
from nicegui import ui
from sovushka.components.charts import _html
from sovushka.state import api_post, last_api_error_text, state
from sovushka.uikit import action_button


class ChatMessages:
    def __init__(self, *,
                 _link_visible_sources,
                 _render_rich_body,
                 _show_source_drawer,
                 _show_sources_artifact,
                 _source_anchor_prefix,
                 _source_markdown,
                 _update_prompt_preview,
                 chat_column,
                 chat_input,
                 detail_dataset,
                 empty_state_ref,
                 send_chat,
                 get_artifacts,
                 ):
        self._link_visible_sources = _link_visible_sources
        self._render_rich_body = _render_rich_body
        self._show_source_drawer = _show_source_drawer
        self._show_sources_artifact = _show_sources_artifact
        self._source_anchor_prefix = _source_anchor_prefix
        self._source_markdown = _source_markdown
        self._update_prompt_preview = _update_prompt_preview
        self.chat_column = chat_column
        self.chat_input = chat_input
        self.detail_dataset = detail_dataset
        self.empty_state_ref = empty_state_ref
        self.send_chat = send_chat
        self.get_artifacts = get_artifacts

    @property
    def artifacts(self):
        return self.get_artifacts()

    def _render_source_tags(self, 
        srcs: list,
        crag: str = "",
        meta: dict | None = None,
        answer: str = "",
    ):
        from sovushka.answer_render import (
            citation_drawer_item,
            citation_sources,
            source_chip,
            source_usage,
        )
        effective_sources = citation_sources(
            srcs,
            (meta or {}).get("source_map") if isinstance(meta, dict) else None,
        )
        if not srcs and not crag and not meta:
            return

        if effective_sources:
            source_anchor_prefix = self._source_anchor_prefix(meta)
            cited_sources = [
                (i, source)
                for i, source in enumerate(effective_sources, 1)
                if source_usage(source, i, answer).get("code") == "used"
            ]
            if cited_sources:
                ui.label("Цитаты в ответе").classes("sov-ui-section-detail")
                with ui.row().classes("gap-1 flex-wrap sov-cited-source-links"):
                    for i, source in cited_sources:
                        c = source_chip(source, i)
                        label = f"Источник {i}"
                        if c["file"]:
                            label += f" · {c['file']}"
                        if c["locator"]:
                            label += f" · {c['locator']}"
                        ui.button(
                            label,
                            icon="o_format_quote",
                            on_click=lambda s=source, n=i: self._show_source_drawer(s, n),
                        ).props(f"flat dense no-caps id={source_anchor_prefix}-{i}").classes(
                            "sov-source-primary sov-ui-source-chip"
                        ).tooltip("Показать цитату и оригинал")
            with ui.expansion(
                f"Источники · {len(effective_sources)}",
                icon="o_library_books",
                value=False,
            ).props("dense").classes("sov-source-expansion"):
                with ui.column().classes("sov-source-list"):
                    for i, source in enumerate(effective_sources, 1):
                        c = source_chip(source, i)
                        item = citation_drawer_item(source, i)
                        lbl = f"{i}. {c['file'] or _source_label(source)}"
                        with ui.row().props(f"id=source-list-{source_anchor_prefix}-{i}").classes(
                            "sov-source-row sov-ui-evidence-card"
                        ):
                            if c["has_ref"]:
                                primary = ui.button(
                                    lbl,
                                    icon="o_description",
                                    on_click=lambda s=source, n=i: self._show_source_drawer(s, n),
                                ).props("flat dense no-caps").classes(
                                    "sov-source-primary sov-ui-source-chip"
                                )
                                primary.tooltip("Показать ссылку и найденный фрагмент")
                            else:
                                primary = ui.label(lbl).classes(
                                    "sov-source-primary sov-source-unavailable sov-ui-source-chip"
                                )
                                primary.tooltip("У источника нет точной ссылки")
            action_button(
                "Источники и цитаты",
                icon="o_format_quote",
                on_click=lambda ss=list(effective_sources), a=answer: self._show_sources_artifact(ss, a),
                variant="secondary",
                compact=True,
                classes="sov-sources-artifact-action",
            )

        with ui.row().classes("msg-srcs sov-source-tools"):
            if crag:
                for chip in _operator_status_chips(crag, meta, srcs):
                    cls = {
                        "ok": "src-tag",
                        "warn": "src-tag src-tag-warn",
                        "err": "src-tag src-tag-err",
                    }.get(chip.get("tone", "dim"), "src-tag")
                    ui.label(chip["label"]).classes(cls)
            if meta:
                tech = _operator_technical_chips(meta)
                if tech:
                    with ui.expansion("Технические детали").props("dense").style(
                        "font-size:.62rem;color:var(--dim);margin-left:4px;"
                    ):
                        with ui.row().classes("gap-1 flex-wrap"):
                            for item in tech:
                                ui.label(item).classes("src-tag")
                history_id = meta.get("history_id")
                if history_id:
                    feedback_buttons: dict[str, object] = {}

                    def _paint_feedback(status: str) -> None:
                        for value, button in feedback_buttons.items():
                            button.classes(
                                remove=(
                                    "sov-answer-feedback__button--active "
                                    "sov-answer-feedback__button--good "
                                    "sov-answer-feedback__button--bad"
                                )
                            )
                            if value == status:
                                tone = "good" if value == "correct" else "bad"
                                button.classes(
                                    add=(
                                        "sov-answer-feedback__button--active "
                                        f"sov-answer-feedback__button--{tone}"
                                    )
                                )

                    async def _feedback(status: str):
                        for button in feedback_buttons.values():
                            button.disable()
                        try:
                            result = await api_post(
                                f"/api/chat/history/{history_id}/feedback",
                                {"feedback": status},
                            )
                            if result:
                                meta["feedback"] = status
                                _paint_feedback(status)
                                ui.notify("Оценка сохранена", type="positive")
                            else:
                                ui.notify(
                                    last_api_error_text("Не удалось сохранить оценку"),
                                    type="warning",
                                )
                        finally:
                            for button in feedback_buttons.values():
                                button.enable()

                    with ui.row().classes("sov-answer-feedback"):
                        ui.label("Ответ полезен?").classes("sov-answer-feedback__label")
                        feedback_buttons["correct"] = ui.button(
                            "Да",
                            icon="thumb_up",
                            on_click=lambda: asyncio.create_task(_feedback("correct")),
                        ).props("flat dense no-caps").classes("sov-answer-feedback__button")
                        feedback_buttons["bad_answer"] = ui.button(
                            "Нет",
                            icon="thumb_down",
                            on_click=lambda: asyncio.create_task(_feedback("bad_answer")),
                        ).props("flat dense no-caps").classes("sov-answer-feedback__button")
                    _paint_feedback(str(meta.get("feedback") or ""))

    def _render_suggestions(self, meta: dict | None):
        if not meta:
            return
        questions = meta.get("clarifying_questions") or []
        filters = meta.get("suggested_filters") or []
        class_suggestions = meta.get("class_suggestions") or []
        if not questions and not filters and not class_suggestions:
            return

        with ui.column().classes("w-full gap-2 mt-2 pt-2 border-t border-dashed border-gray-700"):
            ui.label("Подсказки для уточнения:").classes("text-xs font-semibold text-gray-400 uppercase tracking-wider")

            # ADR-12 мультикласс: вопрос задел несколько классов — предложим переспрос в их области.
            if class_suggestions:
                with ui.row().classes("gap-2 items-center flex-wrap"):
                    ui.label("Посмотреть как:").classes("text-xs text-gray-500")
                    for cs in class_suggestions:
                        def _make_click_class(query=cs.get("query", ""), filt=cs.get("dataset_filter")):
                            async def click_class():
                                if filt and self.detail_dataset.options:
                                    if filt in self.detail_dataset.options:
                                        self.detail_dataset.value = filt
                                    else:
                                        matched = [o for o in self.detail_dataset.options
                                                   if filt.lower() in str(o).lower()]
                                        if matched:
                                            self.detail_dataset.value = matched[0]
                                self.chat_input.value = query
                                self._update_prompt_preview()
                                await self.send_chat()
                            return click_class

                        ui.button(cs.get("label", "?"), on_click=_make_click_class()).props(
                            "outline dense size=sm color=secondary"
                        ).classes("text-xs normal-case")

            if filters:
                with ui.row().classes("gap-2 items-center flex-wrap"):
                    ui.label("Выбрать датасет:").classes("text-xs text-gray-500")
                    for f in filters:
                        f_name = f
                        if f == "NTD_FIRE":
                            f_name = "🔥 Пожарная безопасность"
                        elif f == "NTD_ELECTRICAL":
                            f_name = "⚡ Электрика"
                        elif f == "NTD_STRUCTURAL":
                            f_name = "🏗️ Конструкции"
                        elif f == "TABLE_SMETA":
                            f_name = "📊 Сметы"
                        elif f == "GKRF":
                            f_name = "⚖️ Постановление 87 / ГК РФ"
                        elif f == "NTD":
                            f_name = "📚 Стандарты (СП/ГОСТ)"

                        def _make_click_filter(f_code=f, name=f_name):
                            async def click_filter():
                                if f_code in self.detail_dataset.options:
                                    self.detail_dataset.value = f_code
                                elif name in self.detail_dataset.options:
                                    self.detail_dataset.value = name
                                else:
                                    matched = [opt for opt in self.detail_dataset.options if f_code in opt or f_code.lower() in opt.lower()]
                                    if matched:
                                        self.detail_dataset.value = matched[0]
                                ui.notify(f"Выбран датасет: {name}", type="info")
                                self._update_prompt_preview()
                            return click_filter

                        ui.button(f_name, on_click=_make_click_filter(f, f_name)).props("outline dense size=sm").classes("text-xs text-white border-blue-500")

            if questions:
                with ui.row().classes("gap-2 items-center flex-wrap"):
                    ui.label("Уточнить вопрос:").classes("text-xs text-gray-500")
                    for q in questions:
                        def _make_click_question(q_val=q):
                            async def click_question():
                                self.chat_input.value = q_val
                                self._update_prompt_preview()
                                await self.send_chat()
                            return click_question

                        ui.button(q, on_click=_make_click_question(q)).props("outline dense size=sm color=primary").classes("text-xs text-left normal-case")

    def _render_chat_bubble(self, 
        text: str,
        class_name: str,
        srcs: list | None = None,
        crag: str = "",
        meta: dict | None = None,
    ):
        _mode = (meta or {}).get("out_mode", "text")
        _is_ai = "chat-msg-ai" in class_name
        with ui.element("div").classes(class_name) as bubble:
            if _is_ai:
                _render_model_badge(meta)
                _render_dataset_scope_badge(meta)
                _render_evidence_header(meta, srcs)     # v0.16: статус-полоска сверху
            # AI-ответ с таблицей/диаграммой → рисуем формы прямо в пузыре; SVG и прочее,
            # что inline-рендер не ловит, остаётся на «сыром» тексте + кнопке артефакта.
            explicit_artifact = bool(_artifact_from_meta(meta))
            rich = self._render_rich_body(str(text or ""), srcs or [], meta) if (_is_ai and not explicit_artifact) else False
            if not rich:
                _disp = _bubble_text(str(text or ""), _mode) if (meta and _is_ai) else str(text or "")
                if _is_ai:
                    self._source_markdown(self._link_visible_sources(_disp, srcs or [], meta)).classes("sov-chat-message-text sov-chat-md")
                else:
                    ui.label(_disp).classes("sov-chat-message-text")
            self._render_source_tags(srcs or [], crag, meta, str(text or ""))
            if meta:
                self._render_suggestions(meta)
                if not isinstance(meta.get("source_map"), list) or not meta.get("source_map"):
                    _render_excerpts(meta)
                if _is_ai:
                    self.artifacts._artifact_button(str(text or ""), _mode, meta, srcs or [])
            if _is_ai and str(text or "").strip():
                _render_answer_actions(str(text or ""), srcs or [])
            if _is_ai:
                _render_answer_timing(meta)
            elif meta and meta.get("requested_at"):
                ui.label(format_chat_request_clock(meta["requested_at"])).classes(
                    "sov-chat-timing"
                )
        return bubble

    def _finish_ai_placeholder(self, 
        bubble,
        label,
        text: str,
        srcs: list | None = None,
        crag: str = "",
        error: bool = False,
        meta: dict | None = None,
    ):
        bubble.classes(remove="typing")
        if error:
            bubble.classes(add="chat-msg-error")
        _mode = (meta or {}).get("out_mode", "text")
        with bubble:
            if not error:
                _render_model_badge(meta)
                _render_dataset_scope_badge(meta)
                _render_evidence_header(meta, srcs)     # v0.16: статус-полоска сверху
            # Формы (таблица/mermaid) рисуем виджетами; сырой стрим-label прячем.
            explicit_artifact = bool(_artifact_from_meta(meta))
            rich = self._render_rich_body(str(text or ""), srcs or [], meta) if (meta and not error and not explicit_artifact) else False
            if rich:
                label.set_visibility(False)
            else:
                if meta and not error:
                    label.set_visibility(False)
                    self._source_markdown(self._link_visible_sources(
                        _bubble_text(str(text or ""), _mode), srcs or [], meta
                    )).classes(
                        "sov-chat-message-text sov-chat-md"
                    )
                else:
                    label.set_text(str(text or ""))
            self._render_source_tags(srcs or [], crag, meta, str(text or ""))
            if meta:
                self._render_suggestions(meta)
                if not isinstance(meta.get("source_map"), list) or not meta.get("source_map"):
                    _render_excerpts(meta)
                if not error:
                    self.artifacts._artifact_button(str(text or ""), meta.get("out_mode", "text"), meta, srcs or [])
            if not error and str(text or "").strip():
                _render_answer_actions(str(text or ""), srcs or [])
            if not error:
                _render_answer_timing(meta)

    def _render_msg(self, msg):
        if msg.get("role") == "user":
            self._render_chat_bubble(
                msg.get("text", ""),
                "chat-msg-user",
                meta={"requested_at": msg.get("requested_at")},
            )
            return
        if msg.get("role") == "system":
            self._render_chat_bubble(msg.get("text", ""), "chat-msg-sys")
            return
        self._render_chat_bubble(
            msg.get("text", ""),
            "chat-msg-ai",
            msg.get("srcs", []),
            msg.get("crag", ""),
            msg.get("meta"),
        )
        self.artifacts._register_artifact_downloads(msg.get("meta"))

    def _render_chat_history(self, system_msg: str = "История загружена."):
        self.chat_column.clear()
        self.artifacts._clear_file_artifacts()
        with self.chat_column:
            if not state.get("chat_history") and not state.get("chat_pending"):
                self.empty_state_ref["el"] = _html(
                    '<div class="sov-chat-empty"><div class="sov-chat-empty-title">С чего начнём?</div>'
                    '<div class="sov-chat-empty-copy">Задайте вопрос, приложите документ или выберите источники. '
                    'ЛЕС поможет разобраться и подготовить результат.</div></div>'
                )
            else:
                self._render_chat_bubble(system_msg, "chat-msg-sys")
            for msg in state.get("chat_history", []):
                self._render_msg(msg)
            if state.get("chat_pending"):
                pending_q = state["chat_pending"].get("question", "")
                self._render_chat_bubble(f"Запрос выполняется: {pending_q[:80]}", "chat-msg-ai typing")
