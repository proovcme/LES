"""GUI-first administrator registry for provider-neutral model connections."""
from __future__ import annotations

import asyncio
from functools import partial
from typing import Any

from nicegui import ui
from backend.product_edition import is_light
from sovushka.components.model_setup import open_model_setup
from sovushka.components.model_connection_help import EXAMPLES, build_connection_help

from sovushka.state import api_get, api_post, api_put, last_api_error_text
from sovushka.uikit import action_button, panel, section_heading, select_field, status_badge, text_field
from sovushka.uikit.components import render_feedback_state


_LOCALITY = {
    "loopback": "На этом компьютере",
    "private_network": "В доверенной сети",
    "remote": "Удалённое HTTPS-подключение",
}
_ROLES = {"answer": "Ответы в чате", "embeddings": "Поиск по документам", "local_fallback": "Локальный резерв"}
_CAPS = {"models": "Список моделей", "chat_completions": "Чат", "streaming": "Постепенный вывод ответа", "embeddings": "Эмбеддинги", "tools": "Вызов инструментов", "responses": "Responses API", "structured_output": "Ответ по заданному формату", "token_count": "Подсчёт токенов", "rerank": "Переранжирование"}
_CAP_STATES = {"supported": "подтверждено", "unsupported": "не поддерживается", "unknown": "не подтверждено"}


def connection_title(connection: dict[str, Any]) -> str:
    """Show actual model and service without renaming stored connections."""
    model = str(connection.get("model_id") or "").strip() or "Модель не указана"
    service = {"ollama": "Ollama", "llama_cpp": "llama.cpp", "lm_studio": "LM Studio",
               "lemonade": "Lemonade", "freetoken": "FreeToken", "mlx": "MLX"}.get(
                   connection.get("extension_type"), "Совместимый API")
    return f"{model} · {service}"


def role_connection_title(effective: dict, connections: list[dict]) -> str:
    if not effective:
        return "Не назначено"
    registered = next((item for item in connections if item.get("connection_id")
                       and item.get("connection_id") == effective.get("connection_id")), {})
    return connection_title({**registered, **effective})


def role_limit_text(effective: dict) -> str:
    limit = effective.get("input_token_limit")
    return f"Лимит входного текста в ЛЕС: {limit} токенов" if limit is not None else "Лимит входного текста не определён"


def connection_capability_status(capabilities: list[dict[str, Any]]) -> str:
    return "; ".join(
        f"{_CAPS.get(item.get('name'), 'Дополнительная возможность')}: "
        f"{_CAP_STATES.get(item.get('state'), 'не подтверждено')}"
        for item in capabilities if isinstance(item, dict)
    ) or "ещё не проверялось"


def suggested_connection_name(base_name: str, connections: list[dict[str, Any]]) -> str:
    base = str(base_name or "").strip() or "Подключение"
    occupied = {
        str(item.get("display_name") or "").strip().casefold()
        for item in connections
    }
    if base.casefold() not in occupied:
        return base
    suffix = 2
    while f"{base} {suffix}".casefold() in occupied:
        suffix += 1
    return f"{base} {suffix}"


def connection_save_error(raw_error: str) -> str:
    if "DISPLAY_NAME_IN_USE" in str(raw_error or ""):
        return "Такое название уже используется. Укажите другое название подключения."
    if "model_id" in str(raw_error or "") and "string_too_short" in str(raw_error):
        return "Укажите точное имя модели в поле «Модель»."
    if str(raw_error or "").lstrip().startswith("422"):
        return "Проверьте название подключения, адрес сервиса и имя модели. Подключение не сохранено."
    return "Не удалось сохранить подключение. Проверьте адрес сервиса и повторите попытку; введённые данные сохранены в форме."


def connection_check_summary(payload: dict[str, Any]) -> tuple[str, str]:
    """A completed HTTP probe is not evidence that the selected model works."""
    capabilities = {
        item.get("name"): item.get("state")
        for item in payload.get("capabilities") or [] if isinstance(item, dict)
    }
    available = []
    if capabilities.get("chat_completions") == "supported":
        available.append("ответы в чате")
        if capabilities.get("tools") == "supported":
            available.append("вызов инструментов")
    if capabilities.get("embeddings") == "supported":
        available.append("эмбеддинги для поиска")
    if available:
        if capabilities.get("chat_completions") == "supported" and capabilities.get("tools") != "supported":
            return "warning", (
                "Проверено: " + ", ".join(available) + ". "
                "Вызов инструментов не подтверждён. Для веб-поиска и действий "
                "выберите модель с поддержкой инструментов и нажмите «Проверить»."
            )
        return "positive", "Проверено: " + ", ".join(available) + ". Назначьте подключение для нужной задачи."
    return "warning", "Чат и эмбеддинги не подтверждены. Проверьте адрес, имя модели и ключ; подробности доступны в состоянии проверки."


def build_model_connections():
    data: dict[str, Any] = {
        "connections": [], "bindings": {}, "templates": [], "effective": {}, "qdrant": {},
    }
    refs: dict[str, Any] = {}

    async def _reload() -> None:
        refs["body"].clear()
        with refs["body"]:
            render_feedback_state("loading", detail="Читаю реестр подключений…")
        listing, templates, effective, runtime_registry = await asyncio.gather(
            api_get("/api/model-connections"),
            api_get("/api/model-connections/templates"),
            api_get("/api/model-connections/effective"),
            api_get("/api/settings/runtime-registry"),
        )
        if not isinstance(listing, dict):
            refs["body"].clear()
            with refs["body"]:
                render_feedback_state("error", detail=last_api_error_text("Реестр моделей недоступен"))
            return
        data["connections"] = list(listing.get("connections") or [])
        data["bindings"] = dict(listing.get("bindings") or {})
        data["templates"] = list((templates or {}).get("templates") or []) if isinstance(templates, dict) else []
        data["effective"] = dict((effective or {}).get("roles") or {}) if isinstance(effective, dict) else {}
        factors = list((runtime_registry or {}).get("factors") or []) if isinstance(runtime_registry, dict) else []
        data["qdrant"] = next((item for item in factors if item.get("key") == "QDRANT_URL"), {})
        _render()

    async def _save_qdrant(value: str) -> None:
        result = await api_put(
            "/api/settings/runtime-registry",
            {"updates": {"QDRANT_URL": value}, "danger_confirmations": []},
        )
        if result:
            ui.notify("Адрес Qdrant сохранён", type="positive")
            await _reload()
        else:
            ui.notify(last_api_error_text("Адрес Qdrant не сохранён"), type="negative")

    async def _bind(role: str, connection: dict[str, Any]) -> None:
        previous = data["bindings"].get(role) or {}
        result = await api_put(f"/api/model-connections/roles/{role}", {
            "connection_revision_id": connection["revision_id"],
            "expected_binding_revision": previous.get("binding_revision"),
        })
        if result:
            ui.notify(f"{_ROLES[role]}: назначено {connection_title(connection)}", type="positive")
            await _reload()
        else:
            ui.notify(last_api_error_text("Не удалось назначить подключение"), type="negative")

    async def _test(connection: dict[str, Any], button, feedback) -> None:
        if not button.enabled:
            return
        button.disable()
        button.props("loading")
        feedback.set_text("Проверяем подключение и возможности модели… Это может занять до трёх минут.")
        try:
            result = await asyncio.wait_for(api_post(f"/api/model-connections/{connection['connection_id']}/test", {
                "revision_id": connection["revision_id"],
                "capabilities": ["models", "chat_completions", "streaming", "embeddings", "tools", "responses"],
            }), timeout=180)
            if isinstance(result, dict):
                tone, message = connection_check_summary(result)
                connection["capabilities"] = result.get("capabilities") or []
            else:
                tone, message = "negative", "Сервис не ответил на проверку. Проверьте адрес и запущена ли модель, затем повторите."
            feedback.set_text(message)
            ui.notify(message, type=tone)
        except asyncio.TimeoutError:
            feedback.set_text("Проверка не завершилась за три минуты. Модель может ещё загружаться. Проверьте её состояние и повторите.")
        finally:
            button.props(remove="loading")
            button.enable()

    async def _disable(connection: dict[str, Any]) -> None:
        bound = [role for role, binding in data["bindings"].items() if binding and binding.get("connection_revision_id") == connection["revision_id"]]
        detail = ", ".join(_ROLES.get(role, role) for role in bound)
        ok = await ui.run_javascript(
            "confirm(" + repr(f"Отключить {connection['display_name']} ({connection['base_url']})?" + (f" Назначено: {detail}. Эти назначения перестанут работать до выбора другого подключения." if bound else "")) + ")"
        )
        if not ok:
            return
        result = await api_post(f"/api/model-connections/{connection['connection_id']}/disable", {
            "expected_revision_id": connection["revision_id"], "confirm_bound_roles": bool(bound),
        })
        if result:
            ui.notify("Подключение отключено", type="positive")
            await _reload()
        else:
            ui.notify(last_api_error_text("Не удалось отключить подключение"), type="negative")

    def _open_editor(connection: dict[str, Any] | None = None, *, copy: bool = False, example: str | None = None) -> None:
        current = dict(EXAMPLES[example] if example else connection or {})
        editing = bool(connection and not copy)
        selected_extension = {"value": current.get("extension_type")}
        with ui.dialog() as dialog, ui.card().classes("sov-model-dialog"):
            section_heading("Изменить подключение" if editing else "Новое подключение", "Выберите сервис, укажите адрес и модель. После сохранения проверьте подключение и назначьте его для чата или поиска.")
            template_options = {item["template_id"]: item["display_name"] for item in data["templates"] if not is_light() or item["template_id"] in {"ollama", "openai_compatible"}}
            template = select_field(template_options, label="Сервис", clearable=True, classes="w-full")
            initial_name = current.get("display_name", "")
            if example:
                initial_name = suggested_connection_name(initial_name, data["connections"])
            if copy:
                initial_name = suggested_connection_name(
                    f"{initial_name} — копия",
                    data["connections"],
                )
            name = text_field(label="Название", value=initial_name, classes="w-full")
            endpoint = text_field(label="Адрес сервиса", value=current.get("base_url", ""), classes="w-full")
            ui.label("Шаблон сервиса заполнит адрес. Для другого сервера укажите его совместимый API-адрес.").classes("sov-ui-section-detail")
            model = text_field(label="Модель", value=current.get("model_id", ""), classes="w-full")
            available_models = select_field({}, label="Доступные модели", classes="w-full")
            available_models.props("use-input input-debounce=0")
            available_models.set_visibility(False)
            available_models.on_value_change(lambda event: model.set_value(event.value) if event.value else None)
            discovery_note = ui.label("Получите список с сервера или введите точное имя модели вручную.").classes("sov-ui-section-detail").props('aria-live="polite"')

            async def _discover() -> None:
                address = str(endpoint.value or "").strip()
                location = locality.value
                if not address:
                    endpoint.validate()
                    endpoint.run_method("focus")
                    return
                discover_button.disable()
                discover_button.props("loading")
                available_models.set_visibility(False)
                try:
                    discovery_payload = {"base_url": address, "locality": location}
                    if secret_value is not None and secret_value.value:
                        discovery_payload["secret_value"] = secret_value.value
                    result = await api_post("/api/model-connections/discover-models", discovery_payload)
                    if address != str(endpoint.value or "").strip() or location != locality.value:
                        discovery_note.set_text("Адрес изменён. Получите список для нового сервера.")
                        return
                    if not isinstance(result, dict) or result.get("status") != "ok":
                        discovery_note.set_text((result or {}).get("message") if isinstance(result, dict) and result.get("message") else "Не удалось получить список. Проверьте доступность сервиса или введите имя вручную.")
                        return
                    models = result.get("models") or []
                    available_models.set_options({item: item for item in models}, value=None)
                    available_models.set_visibility(bool(models))
                    discovery_note.set_text("Выберите модель из списка. Она не загружается в память при получении списка." if models else "На сервере нет доступных моделей. Установите модель в Ollama и обновите список.")
                finally:
                    discover_button.enable()
                    discover_button.props(remove="loading")

            discover_button = action_button("Получить модели", icon="o_refresh", on_click=_discover, variant="secondary")
            required_fields = (
                (name, "Введите название подключения."),
                (endpoint, "Укажите адрес сервиса."),
                (model, "Укажите точное имя модели."),
            )
            for field, message in required_fields:
                field.without_auto_validation()
                field.validation = {message: lambda value: bool(str(value or "").strip())}
                field.props(remove="error error-message")
            locality = select_field(_LOCALITY, value=current.get("locality", "loopback"), label="Расположение", classes="w-full")
            def _invalidate_models(_event):
                available_models.set_visibility(False)
                available_models.set_options({}, value=None)
                discovery_note.set_text("Адрес изменён. Получите список моделей для выбранного сервера.")
            endpoint.on_value_change(_invalidate_models)
            locality.on_value_change(_invalidate_models)
            with ui.expansion("Дополнительные параметры", icon="o_tune").classes("w-full"):
                context = ui.number("Размер контекста, токены", value=current.get("requested_context_tokens"), min=1).props("outlined").classes("sov-ui-input w-full")
                ui.label("Оставьте пустым, чтобы использовать доступный для подключения размер контекста.").classes("sov-ui-section-detail")
            secret_value = None
            if editing:
                ui.label("Для замены ключа используйте Ещё → Заменить ключ в карточке подключения.").classes("sov-ui-section-detail")
            else:
                secret_value = text_field(label="Новый ключ", placeholder="Оставьте пустым, если ключ не нужен", classes="w-full").props('type="password"')

            def _template_changed(event) -> None:
                row = next((x for x in data["templates"] if x.get("template_id") == event.value), None)
                if row:
                    selected_extension["value"] = row.get("extension_type")
                    name.set_value(
                        suggested_connection_name(
                            row.get("display_name", ""),
                            data["connections"],
                        )
                    ); endpoint.set_value(row.get("base_url", "")); locality.set_value(row.get("locality", "loopback"))
            template.on_value_change(_template_changed)

            async def _save() -> None:
                invalid = [field for field, _message in required_fields if not field.validate()]
                if invalid:
                    invalid[0].run_method("focus")
                    return
                payload = {"display_name": name.value or "", "base_url": endpoint.value or "", "model_id": model.value or "", "locality": locality.value or "loopback", "requested_context_tokens": int(context.value) if context.value else None, "extension_type": selected_extension["value"]}
                if locality.value != "loopback":
                    ok = await ui.run_javascript("confirm(" + repr(f"Сохранить {_LOCALITY.get(locality.value, locality.value)}: {endpoint.value}?") + ")")
                    if not ok: return
                if editing:
                    payload["expected_revision_id"] = current["revision_id"]
                    result = await api_post(f"/api/model-connections/{current['connection_id']}/revisions", payload)
                else:
                    payload["secret_value"] = secret_value.value or None
                    result = await api_post("/api/model-connections", payload)
                if result:
                    dialog.close(); ui.notify("Подключение сохранено", type="positive"); await _reload()
                else:
                    raw_error = last_api_error_text("Не удалось сохранить подключение")
                    if "DISPLAY_NAME_IN_USE" in raw_error:
                        name.run_method("focus")
                    ui.notify(connection_save_error(raw_error), type="negative")
            with ui.row().classes("justify-end w-full"):
                action_button("Отмена", on_click=dialog.close, variant="quiet")
                action_button("Сохранить", on_click=_save, variant="primary")
        dialog.open()

    def _replace_secret(connection: dict[str, Any]) -> None:
        with ui.dialog() as dialog, ui.card().classes("sov-model-dialog"):
            section_heading("Заменить ключ", connection["display_name"])
            secret_value = text_field(label="Новый ключ", classes="w-full").props('type="password"')
            async def _save() -> None:
                result = await api_post(f"/api/model-connections/{connection['connection_id']}/secret", {"expected_revision_id": connection["revision_id"], "secret_value": secret_value.value or ""})
                if result: dialog.close(); await _reload()
                else: ui.notify(last_api_error_text("Ключ не заменён"), type="negative")
            with ui.row().classes("justify-end w-full"):
                action_button("Отмена", on_click=dialog.close, variant="quiet")
                action_button("Заменить", on_click=_save, variant="primary")
        dialog.open()

    def _render() -> None:
        body = refs["body"]; body.clear()
        with body:
            if is_light():
                with panel(variant="raised"):
                    section_heading("Как подключим модель?", "Локально — на вашем компьютере. Через API — у выбранного вами сервиса.")
                    action_button("Найти на компьютере", icon="o_search", on_click=lambda: open_model_setup(_reload), variant="primary")
                    action_button("Подключить API", icon="o_link", on_click=lambda: open_model_setup(_reload, local=False), variant="secondary")
            build_connection_help(lambda example: _open_editor(example=example), expanded=False)
            qdrant = data.get("qdrant") or {}
            with ui.expansion("Диагностика хранилища", icon="o_storage", value=False).classes("w-full"):
                section_heading(
                    "Хранилище документов",
                    "ЛЕС управляет поисковым индексом автоматически. Ручная настройка не нужна.",
                )
                with ui.row().classes("w-full items-end gap-2"):
                    qdrant_url = text_field(
                        label="Адрес Qdrant",
                        value=str(qdrant.get("display_value") or ("Адрес появится после запуска хранилища" if is_light() else "http://127.0.0.1:6333")),
                        classes="grow",
                    )
                    if is_light():
                        qdrant_url.props("readonly")
                    else:
                        action_button(
                            "Сохранить Qdrant",
                            icon="o_save",
                            on_click=lambda: asyncio.create_task(_save_qdrant(str(qdrant_url.value or ""))),
                            variant="secondary",
                        )
                ui.label(
                    "LES RAG запускает собственное хранилище и выбирает свободный порт автоматически."
                    if is_light() else f"Источник: {qdrant.get('source', 'default')} · применяется после перезапуска ЛЕС"
                ).classes("sov-ui-section-detail")
            with panel(variant="raised", classes="sov-model-summary"):
                section_heading("Готовность к работе", "Для чата нужна модель ответов. Для поиска — модель, которая понимает смысл документов.")
                with ui.row().classes("sov-model-role-grid"):
                    for role, label in _ROLES.items():
                        if is_light() and role == "local_fallback":
                            continue
                        effective = data["effective"].get(role) or {}
                        with panel(variant="inset", classes="sov-model-role"):
                            ui.label(label).classes("sov-ui-section-title")
                            ui.label(role_connection_title(effective, data["connections"])).classes("sov-model-identity")
                            if effective:
                                if role == "answer" and effective.get("status") != "blocked":
                                    status_badge("Назначено для чата", "ok")
                                with ui.expansion("Технические сведения", value=False):
                                    ui.label(role_limit_text(effective)).classes("sov-ui-section-detail")
            connections = data["connections"]
            if not connections:
                render_feedback_state("empty", detail="Создайте первое подключение модели.")
            for connection in connections:
                enabled = bool(connection.get("enabled")); caps = connection.get("capabilities") or []
                with panel(variant="plain", classes="sov-model-connection"):
                    with ui.row().classes("sov-model-connection__head"):
                        with ui.column().classes("grow gap-0"):
                            ui.label(connection_title(connection)).classes("sov-ui-section-title sov-model-identity")
                            ui.label(_LOCALITY.get(connection.get('locality'), 'Другое подключение')).classes("sov-ui-section-detail")
                        status_badge("Включено" if enabled else "Отключено", "ok" if enabled else "blocked")
                        status_badge("Ключ задан" if connection.get("secret_status") == "configured" else "Ключ не требуется" if connection.get("secret_status") == "not_required" else "Ключ не задан", "ok" if connection.get("secret_status") in {"configured", "not_required"} else "warn")
                    check_status = ui.label(connection_check_summary({"capabilities": caps})[1] if caps else "Ещё не проверено. Проверьте модель перед назначением.").classes("sov-model-check-summary").props('role="status" aria-live="polite"')
                    with ui.expansion("Подробности подключения", icon="o_tune").classes("sov-model-details w-full"):
                        ui.label(connection.get("base_url") or "").classes("sov-ui-section-detail")
                        ui.label("Название подключения: " + str(connection.get("display_name") or "Без названия")).classes("sov-ui-section-detail")
                        requested = connection.get("requested_context_tokens") or "автоматически"
                        ui.label(f"Размер контекста: {requested}").classes("sov-ui-section-detail")
                        ui.label("Применится после назначения модели · Перезапуск не требуется").classes("sov-ui-section-detail")
                        ui.label("Состояние проверки: " + connection_capability_status(caps)).classes("sov-ui-section-detail")
                    with ui.row().classes("sov-model-actions"):
                        check_button = action_button("Проверить", icon="o_fact_check", compact=True)
                        check_button.on_click(partial(_test, connection, check_button, check_status))
                        with action_button("Назначить", icon="o_arrow_forward", variant="quiet", compact=True):
                            with ui.menu():
                                for role, role_label in _ROLES.items():
                                    ui.menu_item(role_label, on_click=lambda _e, r=role, c=connection: _bind(r, c))
                        with action_button("Ещё", icon="o_more_horiz", variant="quiet", compact=True):
                            with ui.menu():
                                ui.menu_item("Изменить", on_click=lambda _e, c=connection: _open_editor(c))
                                ui.menu_item("Копировать", on_click=lambda _e, c=connection: _open_editor(c, copy=True))
                                ui.menu_item("Заменить ключ", on_click=lambda _e, c=connection: _replace_secret(c))
                                if enabled:
                                    ui.menu_item("Отключить", on_click=lambda _e, c=connection: _disable(c))

    with ui.column().classes("sov-model-connections-page"):
        with ui.row().classes("sov-model-page-head"):
            section_heading("Модели", "Подключите один раз — затем просто работайте с документами и чатом.")
            with ui.expansion("Дополнительная настройка", value=False):
                action_button("Добавить подключение", icon="o_add", on_click=lambda: _open_editor(), variant="quiet")
        with ui.column().classes("sov-model-connections-body") as body:
            refs["body"] = body
            render_feedback_state("loading", detail="Читаю реестр подключений…")
    ui.timer(0.1, _reload, once=True)
