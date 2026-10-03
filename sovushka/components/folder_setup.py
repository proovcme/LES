"""Connect a folder with retry-safe indexing and optional automatic updates."""
from urllib.parse import quote
from nicegui import ui
from backend.product_edition import is_light
from sovushka.state import api_post, api_put, add_log, last_api_error_text
from sovushka.uikit import action_button, section_heading, text_field

def _dataset_name_from_path(value: str) -> str:
    """Return a useful default name for either a Windows or POSIX folder path."""
    cleaned = str(value or "").strip().strip('"').rstrip("\\/")
    if not cleaned:
        return ""
    return cleaned.replace("\\", "/").rsplit("/", 1)[-1].strip()


def open_folder_setup(add_dialog, *, pick_folder, on_done, initial_path='', on_connected=None):
    _pick_local_folder = pick_folder
    _refresh = on_done
    if add_dialog is None:
        return
    add_dialog.clear()
    picked = {"path": ""}
    auto_name = {"derived": ""}
    with add_dialog, ui.card().classes("sov-folder-connect"):
        with ui.row().classes("w-full items-center justify-between no-wrap"):
            section_heading("Подключить папку", "Документы останутся на своём месте.")
            action_button(icon="o_close", on_click=add_dialog.close, variant="quiet",
                          icon_only=True, aria_label="Закрыть подключение папки")
        name_in = text_field(label="Название", placeholder="Например, документы проекта", classes="w-full")
        with ui.row().classes("sov-folder-connect-path"):
            path_in = text_field(
                label="Папка с документами",
                placeholder="Выберите папку или вставьте полный путь",
                clearable=True, classes="w-full",
            )
            browse_btn = action_button("Выбрать папку", icon="o_folder_open", variant="secondary")
        ui.label("Название подставится из имени папки. Его можно изменить.").classes("sov-ui-section-detail")
        parse_sw = ui.switch("Подготовить документы для поиска", value=True).props("color=positive")
        ui.label("ЛЕС прочитает файлы и добавит их в поиск. Это может занять время.").classes("sov-ui-section-detail")
        watch_sw = ui.switch("Обновлять при изменениях в папке", value=True).props("color=positive")
        ui.label("Пока ЛЕС открыт, новые и изменённые файлы обновляются в поиске автоматически. Исходные файлы не меняются.").classes("sov-ui-section-detail")
        add_error = ui.label("").classes("sov-folder-connect-error").props('role="alert"')
        add_error.set_visibility(False)

        def _add_error(message):
            add_error.set_text(message)
            add_error.set_visibility(bool(message))

        with ui.expansion("Google / Яндекс через веб", icon="o_cloud", value=False).classes("w-full") as cloud_section:
            cloud_section.set_visibility(not is_light())
            ui.label(
                "Укажи ссылку или ID папки Google Drive, либо путь Яндекс Диска вида disk:/Проекты/ПД. "
                "LES скачает папку в mirror-кэш и сделает из неё датасет."
            ).classes("sov-muted")
            with ui.row().classes("items-center w-full").style("gap:8px;"):
                cloud_provider = ui.select(
                    options={"google_drive": "Google Drive", "yandex_disk": "Яндекс Диск"},
                    value="google_drive",
                    label="Провайдер",
                ).props("dense outlined emit-value map-options").style("min-width:170px;")
                cloud_locator = ui.input(
                    "Ссылка / ID / путь папки",
                    placeholder="https://drive.google.com/drive/folders/... или disk:/Проекты/ПД",
                ).props("dense outlined clearable").classes("flex-1")

            async def _do_cloud_add():
                nm = (name_in.value or "").strip()
                locator = (cloud_locator.value or "").strip()
                if not nm or not locator:
                    ui.notify("Нужны название датасета и ссылка/путь облачной папки", type="warning")
                    return
                payload = {
                    "provider": cloud_provider.value,
                    "locator": locator,
                    "dataset_name": nm,
                    "parse": bool(parse_sw.value),
                    "parse_limit": 25,
                    "max_files": 500,
                    "background": True,
                }
                d = await api_post("/api/rag/cloud-drives/sync", payload)
                if d and d.get("status") in {"started", "registered"}:
                    add_dialog.close()
                    ui.notify(f"Облачная папка «{nm}» подключается как датасет", type="positive")
                    add_log(f"[CLOUD_DRIVE] {cloud_provider.value}: {locator} → «{nm}»")
                    await _refresh()
                else:
                    ui.notify(last_api_error_text("Не удалось подключить облачную папку"), type="negative")

            with ui.row().classes("justify-end w-full").style("gap:8px;margin-top:6px;"):
                action_button(
                    "Подключить веб-папку",
                    icon="o_cloud_sync",
                    on_click=_do_cloud_add,
                    variant="primary",
                    compact=True,
                )

        def _set_path(value: str) -> None:
            path = str(value or "").strip().strip('"')
            old_derived = auto_name["derived"]
            derived = _dataset_name_from_path(path)
            picked["path"] = path
            auto_name["derived"] = derived
            path_in.value = path
            path_in.update()
            current_name = str(name_in.value or "").strip()
            if derived and (not current_name or current_name == old_derived):
                name_in.value = derived
                name_in.update()

        async def _open_native_folder(*_event_args):
            if picked.get("browsing") or picked.get("intake_result"):
                return
            picked["browsing"] = True
            browse_btn.disable()
            browse_btn.props("loading")
            _add_error("")
            try:
                path = await _pick_local_folder(
                    initial=str(path_in.value or ""), title="Выберите папку с документами",
                )
                if path:
                    _set_path(path)
            except Exception:
                _add_error("Не удалось открыть выбор папки. Вставьте путь из Проводника в поле выше.")
            finally:
                picked["browsing"] = False
                browse_btn.enable()
                browse_btn.props(remove="loading")

        browse_btn.on("click", _open_native_folder)
        path_in.on("update:model-value", lambda event: _set_path(str(event.args or "")))

        async def _submit_add():
            _add_error("")
            nm = (name_in.value or "").strip()
            pth = str(path_in.value or picked["path"] or "").strip().strip('"')
            if not nm or not pth:
                _add_error("Введите название и выберите папку с документами.")
                return
            plan = await api_post("/api/rag/external/intake-plan", {"path": pth, "dataset_name": nm})
            if not isinstance(plan, dict) or plan.get("status") != "ok":
                _add_error(last_api_error_text("Не удалось прочитать папку. Проверьте путь и доступ к ней."))
                return
            if plan.get('accepted_count') == 0:
                detail = 'Папка пока пуста.' if plan.get('source_state') == 'empty' else 'Поддерживаемых документов пока нет.'
                if not watch_sw.value:
                    _add_error(detail + ' Включите наблюдение, чтобы подхватить новые документы, или выберите другую папку.')
                    return
                if picked.get('empty_confirmed_for') != pth:
                    picked['empty_confirmed_for'] = pth
                    add_button.set_text('Подключить под наблюдение')
                    add_button.props('aria-label="Подключить под наблюдение"')
                    _add_error(detail + ' Поиск пока недоступен. Нажмите «Подключить под наблюдение», чтобы подхватить будущие файлы.')
                    return
            did = picked.get("dataset_id")
            if not did:
                ds = await api_post("/api/rag/datasets", {"name": nm})
                did = (ds or {}).get("id")
                if did:
                    picked["dataset_id"] = did
                    name_in.disable()
            if not did:
                _add_error(last_api_error_text("Не удалось подключить папку. Попробуйте ещё раз."))
                return
            # background=True: index-external отвечает мгновенно, регистрация+нарезка+парс — в фоне
            # (большие папки не упираются в HTTP-таймаут 180с; 758 файлов = ~47с регистрации).
            r = picked.get("intake_result")
            if not r:
                r = {'status': 'registered', 'empty': True} if plan.get('accepted_count') == 0 else await api_post(
                    "/api/rag/index-external", {"path": pth, "dataset_id": did, "parse": bool(parse_sw.value),
                                              "parse_limit": 25, "background": True})
            if r and r.get("status") in ("started", "registered"):
                picked["intake_result"] = r
                browse_btn.disable()
                path_in.disable()
                parse_sw.disable()
                if watch_sw.value:
                    watch_result = await api_put(f"/api/rag/datasets/{quote(str(did), safe='')}/watch",
                        {"path": pth, "enabled": True, "auto_index": bool(parse_sw.value)})
                    if not isinstance(watch_result, dict) or not watch_result.get("watch", {}).get("enabled"):
                        _add_error("Папка подключена, но автообновление не включилось. Повторите попытку или отключите обновление — подготовка документов продолжится.")
                        await _refresh()
                        return
                add_dialog.close()
                parse_job = r.get("parse_job") if isinstance(r.get("parse_job"), dict) else {}
                if parse_job.get("job_id"):
                    add_log(
                        f"[EXT_INDEX] «{nm}»: parse job {parse_job.get('job_id')} · "
                        f"batch {parse_job.get('batch_limit')} · max {parse_job.get('max_batches')}"
                    )
                ui.notify(
                    f"Папка «{nm}» подключена. " + (
                        "Ожидаю появления документов." if r.get('empty') else "Документы готовятся для поиска." if parse_sw.value
                        else "Подготовку для поиска можно запустить позже."
                    ), type="positive")
                await _refresh()
                if on_connected:
                    await on_connected({'attachment_id': did, 'mode': 'index', 'kind': 'folder',
                                        'name': nm, 'dataset_name': nm, 'status': 'queued'})
            else:
                _add_error(last_api_error_text("Подготовка не подтверждена. Набор сохранён; повторите попытку — дубликат не создастся."))
                await _refresh()

        async def _do_add():
            if picked.get("submitting"):
                return
            picked["submitting"] = True
            add_button.disable()
            add_button.props("loading")
            try:
                await _submit_add()
            finally:
                picked["submitting"] = False
                add_button.enable()
                add_button.props(remove="loading")

        with ui.row().classes("justify-end w-full").style("gap:8px;margin-top:8px;"):
            action_button(
                "Отмена",
                on_click=add_dialog.close,
                variant="quiet",
                compact=True,
            )
            add_button = action_button(
                "Подключить папку",
                icon="o_folder_open",
                on_click=_do_add,
                variant="primary",
            )
    if initial_path:
        _set_path(initial_path)
    add_dialog.open()
