"""Two short setup paths sharing explicit, checked role assignment."""
import asyncio
import ipaddress
from urllib.parse import urlsplit

from nicegui import ui
from sovushka.state import api_get, api_post, api_put
from sovushka.uikit import action_button, section_heading, select_field, text_field

ROLES = {"answer": "Ответы в чате", "embeddings": "Поиск по документам"}


def endpoint_locality(address):
    try:
        host = (urlsplit(address).hostname or "").lower()
    except ValueError:
        return "remote"
    if host == "localhost":
        return "loopback"
    try:
        ip = ipaddress.ip_address(host)
        return "loopback" if ip.is_loopback else "private_network" if ip.is_private else "remote"
    except ValueError:
        return "remote"


def open_model_setup(on_done, *, local=True):
    engines, saved = {}, {}
    with ui.dialog() as dialog, ui.card().classes("sov-model-dialog"):
        section_heading("Модель на компьютере" if local else "Подключить API",
                        "Выберите модель и её задачу. ЛЕС проверит подключение перед назначением.")
        note = ui.label("Ищем работающие локальные серверы…" if local else
                        "Укажите адрес API вашего сервиса. Ключ нужен только если сервис его требует.")
        note.classes("sov-ui-section-detail").props('role="status" aria-live="polite"')
        engine = select_field({}, label="Найденный движок", classes="w-full")
        engine.set_visibility(False)
        address = text_field(label="Адрес API", placeholder="https://…/v1", classes="w-full")
        address.set_visibility(not local)
        secret = text_field(label="API-ключ (если требуется)", classes="w-full").props('type="password" autocomplete="off"')
        secret.set_visibility(not local)
        model = select_field({}, label="Модель", classes="w-full").props("use-input input-debounce=0")
        model.set_visibility(False)
        manual = text_field(label="Имя модели", classes="w-full")
        manual.set_visibility(False)
        role = select_field(ROLES, value="answer", label="Для чего использовать", classes="w-full")
        ui.label("Для чата нужна текстовая модель, для поиска — embedding-модель. Назначение поиска не создаёт и не переносит индекс: модель и размерность должны совпадать с его настройками.").classes("sov-ui-section-detail")
        privacy = ui.label("При использовании внешнего API ваши запросы и выбранные фрагменты документов передаются этому сервису.").classes("sov-ui-section-detail")
        privacy.set_visibility(not local)

        def current():
            if local:
                chosen = engines.get(engine.value) or {}
                return chosen.get("base_url", ""), chosen.get("extension_type"), "loopback"
            value = str(address.value or "").strip().rstrip("/")
            return value, None, endpoint_locality(value)

        def selected(*_):
            connect.set_enabled(bool(current()[0] and (model.value or (manual.value if manual.visible else ""))))

        def choose_engine(*_):
            available = (engines.get(engine.value) or {}).get("models", [])
            model.set_options({value: value for value in available}, value=None)
            model.set_visibility(bool(available))
            note.set_text("Выберите модель и нажмите «Проверить и подключить»." if available else
                          "Движок найден, но моделей нет. Загрузите модель в нём и повторите поиск.")
            selected()

        def changed_address(*_):
            saved.clear()
            model.set_options({}, value=None)
            model.set_visibility(False)
            selected()

        async def discover():
            find.disable(); find.props("loading"); connect.disable()
            try:
                if local:
                    result = await api_post("/api/model-connections/discover-local", {})
                    rows = result.get("engines", []) if isinstance(result, dict) else []
                    engines.clear(); engines.update({row["id"]: row for row in rows})
                    engine.set_options({row["id"]: row["name"] for row in rows}, value=rows[0]["id"] if len(rows) == 1 else None)
                    engine.set_visibility(bool(rows))
                    model.set_options({}, value=None); model.set_visibility(False)
                    if len(rows) == 1:
                        choose_engine()
                    else:
                        note.set_text("Выберите движок, затем модель." if rows else
                                      "Работающие серверы не найдены. Запустите Ollama, LM Studio или другой сервер и повторите поиск. Для нестандартного порта используйте «Подключить API».")
                else:
                    base, _, locality = current()
                    if not base:
                        note.set_text("Укажите адрес API."); return
                    requested_secret = str(secret.value or "")
                    payload = {"base_url": base, "locality": locality}
                    if requested_secret:
                        payload["secret_value"] = requested_secret
                    note.set_text("Получаем список моделей…")
                    result = await api_post("/api/model-connections/discover-models", payload)
                    if current()[0] != base or str(secret.value or "") != requested_secret:
                        note.set_text("Адрес или ключ изменились. Повторите получение моделей."); return
                    values = result.get("models", []) if isinstance(result, dict) and result.get("status") == "ok" else []
                    model.set_options({value: value for value in values}, value=None)
                    model.set_visibility(bool(values))
                    manual.set_visibility(not values)
                    note.set_text("Выберите модель из списка." if values else
                                  (result or {}).get("message", "Список моделей пуст. Введите точное имя модели из вашего сервиса."))
            except Exception:
                note.set_text("Не удалось получить список. Проверьте подключение и повторите поиск.")
            finally:
                find.enable(); find.props(remove="loading"); selected()

        async def connect_model():
            if not connect.enabled:
                return
            base, extension, locality = current()
            chosen = str(model.value or (manual.value if manual.visible else "") or "").strip()
            purpose = role.value
            if not base or not chosen:
                note.set_text("Выберите модель."); return
            controls = (connect, find, engine, address, secret, model, manual, role, close)
            for control in controls: control.disable()
            dialog.props("persistent"); connect.props("loading")
            note.set_text("Проверяем модель. Первый запуск может занять несколько минут…")
            try:
                listing = await api_get("/api/model-connections")
                if not isinstance(listing, dict):
                    note.set_text("Не удалось прочитать настройки. Повторите попытку."); return
                key = (base, chosen)
                connection = saved.get(key)
                if not connection and not secret.value:
                    connection = next((c for c in listing.get("connections", []) if
                        c.get("base_url", "").rstrip("/") == base and c.get("model_id") == chosen and c.get("enabled", True)), None)
                if not connection:
                    names = {c.get("display_name") for c in listing.get("connections", [])}
                    name = chosen; index = 2
                    while name in names:
                        name = f"{chosen} ({index})"; index += 1
                    payload = {"display_name": name, "base_url": base, "model_id": chosen,
                               "locality": locality, "extension_type": extension}
                    if not local and secret.value: payload["secret_value"] = secret.value
                    connection = await api_post("/api/model-connections", payload)
                if not isinstance(connection, dict) or not connection.get("revision_id"):
                    note.set_text("Не удалось сохранить подключение. Проверьте адрес и имя модели. Введённые значения сохранены."); return
                saved[key] = connection
                previous = (listing.get("bindings") or {}).get(purpose) or {}
                result = await asyncio.wait_for(api_put(f"/api/model-connections/roles/{purpose}", {
                    "connection_revision_id": connection["revision_id"],
                    "expected_binding_revision": previous.get("binding_revision"),
                }), timeout=240)
                if not isinstance(result, dict) or not result.get("connection_revision_id"):
                    note.set_text("Модель не прошла проверку для выбранной задачи. Проверьте доступ к сервису или выберите другую модель. Прежнее назначение сохранено."); return
                note.set_text(f"Подключено: {chosen} — {ROLES[purpose].lower()}.")
                secret.set_value("")
                await on_done()
            except asyncio.TimeoutError:
                note.set_text("Проверка занимает больше времени. Подключение сохранено; проверьте его состояние перед повтором.")
            except Exception:
                note.set_text("Подключение прервалось. Проверьте состояние модели и повторите попытку.")
            finally:
                for control in controls: control.enable()
                dialog.props(remove="persistent"); connect.props(remove="loading"); selected()

        find = action_button("Найти на компьютере" if local else "Получить модели", icon="o_search", on_click=discover, variant="secondary")
        with ui.row().classes("w-full justify-end gap-2"):
            close = action_button("Закрыть", on_click=dialog.close, variant="quiet")
            connect = action_button("Проверить и подключить", on_click=connect_model, variant="primary")
            connect.disable()
        engine.on_value_change(choose_engine)
        model.on_value_change(selected); manual.on_value_change(selected)
        address.on_value_change(changed_address); secret.on_value_change(lambda *_: saved.clear())
    dialog.open()
    if local:
        ui.timer(0.1, discover, once=True)
    return dialog
