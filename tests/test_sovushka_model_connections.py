import inspect
import pytest


def test_connection_help_renders_examples_without_assigning_a_model():
    from nicegui import Client
    from nicegui.page import page
    from sovushka.components.model_connection_help import build_connection_help, EXAMPLES

    callbacks = []
    with Client(page("/__connection_help_test")) as client:
        build_connection_help(callbacks.append, expanded=True)
        texts = [str(getattr(element, "text", "")) for element in client.elements.values()]
        assert "Где взять модель и как подключить" in texts
        assert "Пример для чата" in texts
        assert "Пример для поиска" in texts
        assert callbacks == []
        buttons = [element for element in client.elements.values() if getattr(element, "text", "") == "Пример для чата"]
        assert len(buttons) == 1
        listener = next(iter(buttons[0]._event_listeners.values()))
        listener.handler(None)
        assert callbacks == ["chat"]
    assert EXAMPLES["chat"]["base_url"] == "http://127.0.0.1:11434/v1"
    assert EXAMPLES["search"]["model_id"] == "bge-m3"
    assert EXAMPLES["chat"]["extension_type"] == "ollama"
    assert all("secret_value" not in example for example in EXAMPLES.values())


def test_probe_completion_does_not_claim_unsupported_model_is_ready():
    from sovushka.pages.model_connections import connection_check_summary
    tone, message = connection_check_summary({"capabilities": [
        {"name": "models", "state": "supported"},
        {"name": "chat_completions", "state": "unsupported"},
        {"name": "embeddings", "state": "unknown"},
    ]})
    assert tone == "warning"
    assert "не подтверждены" in message


def test_probe_reports_only_the_supported_role():
    from sovushka.pages.model_connections import connection_check_summary
    tone, message = connection_check_summary({"capabilities": [
        {"name": "chat_completions", "state": "supported"},
        {"name": "tools", "state": "supported"},
        {"name": "embeddings", "state": "unsupported"},
    ]})
    assert tone == "positive"
    assert "ответы в чате" in message
    assert "эмбеддинги для поиска" not in message


@pytest.mark.parametrize("tools_state", [None, "unknown", "unsupported"])
def test_chat_success_keeps_missing_tools_visible(tools_state):
    from sovushka.pages.model_connections import connection_check_summary
    capabilities = [{"name": "chat_completions", "state": "supported"}]
    if tools_state is not None:
        capabilities.append({"name": "tools", "state": tools_state})
    tone, message = connection_check_summary({"capabilities": capabilities})
    assert tone == "warning"
    assert "Проверено: ответы в чате" in message
    assert "Вызов инструментов не подтверждён" in message
    assert "«Проверить»" in message


def test_embedding_only_connection_does_not_require_chat_tools():
    from sovushka.pages.model_connections import connection_check_summary
    tone, message = connection_check_summary({"capabilities": [
        {"name": "embeddings", "state": "supported"},
        {"name": "chat_completions", "state": "unsupported"},
        {"name": "tools", "state": "unsupported"},
    ]})
    assert tone == "positive"
    assert "эмбеддинги для поиска" in message
    assert "Вызов инструментов не подтверждён" not in message


def test_capability_details_use_readable_states_and_handle_absent_results():
    from sovushka.pages.model_connections import connection_capability_status
    assert connection_capability_status([]) == "ещё не проверялось"
    text = connection_capability_status([
        {"name": "chat_completions", "state": "supported"},
        {"name": "tools", "state": "unsupported"},
        {"name": "streaming", "state": "unknown"},
    ])
    assert "Чат: подтверждено" in text
    assert "Вызов инструментов: не поддерживается" in text
    assert "Постепенный вывод ответа: не подтверждено" in text
    assert "unsupported" not in text
from pathlib import Path


def test_model_connection_names_are_unique_before_submit():
    from sovushka.pages.model_connections import (
        connection_save_error,
        suggested_connection_name,
    )

    connections = [
        {"display_name": "Ollama"},
        {"display_name": "Ollama 2"},
        {"display_name": "Ответы 14B"},
    ]

    assert suggested_connection_name("MLX", connections) == "MLX"
    assert suggested_connection_name("Ollama", connections) == "Ollama 3"
    assert connection_save_error("409: {'code': 'DISPLAY_NAME_IN_USE'}") == (
        "Такое название уже используется. Укажите другое название подключения."
    )
    message = connection_save_error("422: [{'type': 'string_too_short', 'loc': ['body', 'model_id']}]")
    assert 'имя модели' in message
    assert '422' not in message and 'model_id' not in message


def test_model_connections_page_uses_safe_registry_actions():
    from sovushka.pages.model_connections import build_model_connections

    source = inspect.getsource(build_model_connections)
    for label in (
        "Модели",
        "Найти на компьютере",
        "Подключить API",
        "Проверить",
        "Назначить",
        "Изменить",
        "Копировать",
        "Заменить ключ",
        "Отключить",
    ):
        assert label in source
    assert 'type="password"' in source
    assert "api_key" not in source
    assert "secret_ref" not in source
    assert "/api/model-connections" in source
    assert "panel(" in source
    assert "status_badge(" in source
    assert 'classes("sov-model-connections-page")' in source
    assert 'name.run_method("focus")' in source


def test_model_page_explains_locality_context_source_and_restart():
    source = Path("sovushka/pages/model_connections.py").read_text(encoding="utf-8")
    for label in (
        "На этом компьютере",
        "В доверенной сети",
        "Удалённое HTTPS-подключение",
        "Размер контекста",
        "Применится после назначения модели",
        "Перезапуск не требуется",
        "Состояние проверки",
    ):
        assert label in source
    assert "confirm(" in source
    assert "Эти назначения перестанут работать до выбора другого подключения" in source
    assert "BOUND_CONNECTION" not in source
    assert "capability/preset" not in source


def test_model_page_describes_assignment_without_claiming_a_live_answer():
    source = Path("sovushka/pages/model_connections.py").read_text(encoding="utf-8")

    assert "Назначено для чата" in source
    assert "Работает в чате" not in source
    assert "Назначено, но не используется" not in source


def test_qdrant_connection_remains_available_in_collapsed_diagnostics():
    source = Path("sovushka/pages/model_connections.py").read_text(encoding="utf-8")

    assert 'api_get("/api/settings/runtime-registry")' in source
    assert 'item.get("key") == "QDRANT_URL"' in source
    assert 'ui.expansion("Диагностика хранилища"' in source
    assert 'ui.expansion("Дополнительная настройка"' in source
    assert 'label="Адрес Qdrant"' in source
    assert '"Сохранить Qdrant"' in source
    assert '"updates": {"QDRANT_URL": value}' in source


def test_configuration_navigation_has_model_connections_tab():
    header = Path("sovushka/components/header.py").read_text(encoding="utf-8")
    shell = Path("sovushka_ng.py").read_text(encoding="utf-8")

    assert 'ui.tab("Модели"' in header
    assert 'tab_refs["model_connections"]' in header
    assert "build_model_connections" in shell
    assert '"Модели": tab_model_connections' in shell


def test_legacy_settings_points_to_registry_instead_of_provider_fields():
    header = Path("sovushka/components/header.py").read_text(encoding="utf-8")

    assert "Конфигурация → Модели" in header
    assert 'ui.navigate.to("/les/classic?tab=models")' in header
    assert "on_click=lambda: settings_dialog.open()" not in header
