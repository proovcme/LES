"""Evidence panel: full excerpt, original and return to the source list."""
from nicegui import ui
from sovushka.components.charts import _html, esc
from sovushka.state import api_post
from sovushka.uikit import action_button, panel

def build_source_panels(artifact_panel, open_artifacts, copy_button):
    _open_artifacts = open_artifacts
    _copy_button = copy_button
    def _show_source_drawer(source, index: int, *, source_list=None, source_answer="") -> None:
        """v0.23B: source chip opens a real citation drawer in Artifacts."""
        from sovushka.answer_render import citation_drawer_item

        item = citation_drawer_item(source, index)
        _open_artifacts()
        artifact_panel.clear()
        with artifact_panel:
            if source_list is not None:
                action_button(
                    "Все источники ответа", icon="o_arrow_back", variant="quiet",
                    on_click=lambda: _show_sources_artifact(source_list, source_answer),
                )
            with ui.card().classes("sov-artifact-card"):
                with ui.row().classes("w-full items-center justify-between gap-2"):
                    ui.label(f"Источник {item.get('n') or index}").classes("sov-panel-title")
                    if item.get("kind"):
                        ui.label(str(item["kind"])).classes(
                            "src-tag src-tag-warn" if item.get("weak") else "src-tag"
                        )
                title = str(item.get("file") or "источник")
                if item.get("locator"):
                    title += f" · {item['locator']}"
                ui.label(title).classes("sov-source-detail-title")
                if item.get("snippet"):
                    with panel(variant="inset", classes="sov-source-excerpt"):
                        ui.label("Цитата из документа").classes("sov-ui-section-detail")
                        ui.label(str(item["snippet"])).classes("sov-source-excerpt-text")
                if item.get("viewer_url"):
                    viewer_url = esc(str(item["viewer_url"]))
                    _html(
                        '<div class="sov-embedded-file-viewer">'
                        f'<iframe src="{viewer_url}" title="Просмотр источника" loading="eager" '
                        'sandbox="allow-scripts allow-same-origin allow-popups"></iframe>'
                        '</div>'
                    )
                if item.get("has_auditable_locator"):
                    source_ref_val = str(item.get("source_ref") or item.get("copy_text") or "")
                    with ui.row().classes("gap-2 items-center flex-wrap").style("margin-top:6px;"):
                        citation_text = f"{title}\n{item.get('snippet') or source_ref_val}".strip()
                        _copy_button("Цитату", citation_text, icon="o_format_quote", classes="sov-answer-act")
                        
                        async def _do_native_open(native_url=str(item.get("native_open_url") or "")) -> None:
                            data = await api_post(native_url) if native_url else None
                            if isinstance(data, dict) and data.get("status") == "opened":
                                ui.notify("Файл открыт в системном приложении", type="positive")
                            else:
                                err_msg = str((data or {}).get("error") or "Не удалось открыть файл в системе")
                                ui.notify(err_msg, type="warning")

                        if item.get("native_open_url"):
                            ui.button("Открыть оригинал", on_click=_do_native_open).classes("sov-answer-act")
                        if item.get("viewer_url"):
                            ui.link("Просмотр", str(item["viewer_url"])).props("target=_blank").classes("sov-answer-act")
                        if item.get("open_url"):
                            ui.link("Скачать", str(item["open_url"])).props("target=_blank").classes("sov-answer-act")
                        if item.get("norm_card_url"):
                            ui.link("Карточка нормы", str(item["norm_card_url"])).props("target=_blank").classes("sov-answer-act")
                        if item.get("web_url"):
                            ui.link("Открыть веб-источник", str(item["web_url"])).props("target=_blank").classes("sov-answer-act")
                        if not any(
                            item.get(key)
                            for key in ("native_open_url", "viewer_url", "open_url", "norm_card_url", "web_url")
                        ):
                            ui.label(str(item.get("unavailable_reason") or "Открытие недоступно")).classes(
                                "src-tag src-tag-warn"
                            )
                    if item.get("source_ref") or item.get("relative_path"):
                        with ui.expansion("Расположение и технические сведения").props("dense").classes(
                            "sov-source-technical"
                        ):
                            reference = str(item.get("source_ref") or item.get("relative_path"))
                            ui.label(reference).classes("sov-source-technical__ref")
                            _copy_button("Копировать ссылку", reference, classes="sov-answer-act")
                else:
                    ui.label(str(item.get("unavailable_reason") or "Нет source_ref")).classes("src-tag src-tag-warn")

    def _show_sources_artifact(sources: list, answer: str = "") -> None:
        """Render all prompt-visible citations as one auditable artifact."""
        from sovushka.answer_render import answer_copy_text, citation_drawer_item

        _open_artifacts()
        artifact_panel.clear()
        with artifact_panel:
            with ui.card().classes("sov-artifact-card"):
                ui.label("Источники ответа").classes("sov-panel-title")
                ui.label(
                    f"Документы и фрагменты: {len(sources)}. Откройте цитату, чтобы проверить ответ."
                ).classes("sov-ui-section-detail")
                with ui.column().classes("sov-source-list"):
                    for index, source in enumerate(sources, 1):
                        item = citation_drawer_item(source, index)
                        title = str(item.get("file") or f"Источник {index}")
                        if item.get("locator"):
                            title += f" · {item['locator']}"
                        with ui.element("section").classes("sov-source-row sov-ui-evidence-card"):
                            ui.label(f"Источник {index}").classes("sov-ui-section-detail")
                            action_button(
                                title, icon="o_description", variant="quiet",
                                on_click=lambda s=source, n=index: _show_source_drawer(
                                    s, n, source_list=sources, source_answer=answer,
                                ),
                                classes="sov-source-preview-button",
                            ).tooltip("Показать фрагмент рядом с ответом")
                            target = str(item.get("viewer_url") or item.get("web_url") or item.get("open_url") or "")
                            if target:
                                ui.link("Открыть источник ↗", target, new_tab=True).props(
                                    'rel="noopener noreferrer"'
                                ).classes("sov-source-open-link")
                            if item.get("snippet"):
                                ui.label(str(item["snippet"])).classes("sov-source-excerpt-text sov-source-list-excerpt")
                _copy_button(
                    "Скопировать с источниками",
                    answer_copy_text(answer, sources, with_sources=True),
                    icon="o_content_copy",
                    classes="sov-answer-act",
                )

    return _show_source_drawer, _show_sources_artifact
