"""Actual NiceGUI callbacks, without a browser; no visual acceptance claims."""
import inspect
import asyncio
import ast
from pathlib import Path

from nicegui import Client, ui
from nicegui.page import page
import pytest


@pytest.mark.asyncio
async def test_model_cards_render_real_identity_and_keep_diagnostics_collapsed(monkeypatch):
    from sovushka.pages import model_connections as module
    timers = []
    connection = {"connection_id": "search", "model_id": "bge-m3:latest",
                  "display_name": "Ollama 3", "extension_type": "ollama", "enabled": True,
                  "capabilities": [], "locality": "loopback"}
    async def get(route):
        if route == '/api/model-connections':
            return {"connections": [connection]}
        if route == '/api/model-connections/effective':
            return {"roles": {"embeddings": {**connection, "preset_id": "internal-profile", "input_token_limit": 6000}}}
        return {}
    monkeypatch.setattr(module, 'api_get', get)
    monkeypatch.setattr(ui, 'timer', lambda interval, callback, **kwargs: timers.append(callback))
    with Client(page('/__model_identity')) as client:
        module.build_model_connections()
        await timers[0]()
        labels = [str(getattr(e, 'text', '')) for e in client.elements.values()]
        assert labels.count('bge-m3:latest · Ollama') == 2
        assert not any('internal-profile' in text for text in labels)
        details = next(e for e in client.elements.values() if isinstance(e, ui.expansion)
                       and e._props.get('label') == 'Подробности подключения')
        assert not details.value
        assert 'Название подключения: Ollama 3' in labels


async def click(element):
    handlers = [event.handler for event in element._event_listeners.values() if event.type == "click"]
    assert handlers, "The rendered action must have a click handler"
    for handler in handlers:
        result = handler(None) if inspect.signature(handler).parameters else handler()
        if inspect.isawaitable(result):
            await result
    # NiceGUI's click wrapper schedules async callbacks on the event loop.
    await asyncio.sleep(0)


@pytest.mark.asyncio
@pytest.mark.parametrize('outcome', ['models', 'empty', 'failure'])
async def test_model_discovery_selects_without_saving_or_losing_manual_name(monkeypatch, outcome):
    from nicegui import core
    from sovushka.pages import model_connections as module
    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    timers, requests = [], []
    async def get(route):
        return {}
    async def post(route, payload):
        requests.append((route, payload))
        return {'status': 'unavailable', 'message': 'Сервис недоступен'} if outcome == 'failure' else {'status': 'ok', 'models': ['test:1', 'test:2'] if outcome == 'models' else []}
    monkeypatch.setattr(module, 'api_get', get)
    monkeypatch.setattr(module, 'api_post', post)
    monkeypatch.setattr(ui, 'timer', lambda interval, callback, **kwargs: timers.append(callback))
    with Client(page('/__model_discovery')) as client:
        module.build_model_connections()
        await timers[0]()
        button = lambda label: next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == label)
        await click(button('Добавить подключение'))
        fields = {e._props['label']: e for e in client.elements.values() if 'label' in e._props and hasattr(e, 'set_value')}
        fields['Адрес сервиса'].set_value('http://127.0.0.1:11434/v1')
        fields['Модель'].set_value('manual:1')
        await click(button('Получить модели'))
        assert requests == [('/api/model-connections/discover-models', {'base_url': 'http://127.0.0.1:11434/v1', 'locality': 'loopback'})]
        assert fields['Модель'].value == 'manual:1'
        available = fields['Доступные модели']
        if outcome == 'models':
            assert available.visible
            available.set_value('test:2')
            assert fields['Модель'].value == 'test:2'
            fields['Адрес сервиса'].set_value('http://127.0.0.1:11435/v1')
            assert not available.visible
        else:
            assert not available.visible


def test_dataset_path_placeholder_does_not_expose_a_named_home():
    from sovushka.components import folder_setup as samovar
    tree = ast.parse(Path(samovar.__file__).read_text(encoding='utf-8'))
    placeholders = [keyword.value.value for node in ast.walk(tree)
                    if isinstance(node, ast.Call) for keyword in node.keywords
                    if keyword.arg == 'placeholder' and isinstance(keyword.value, ast.Constant)
                    and isinstance(keyword.value.value, str)]
    assert 'Выберите папку или вставьте полный путь' in placeholders
    assert not any('\\Users\\' in value or '/Users/' in value or '/home/' in value
                   for value in placeholders)


@pytest.mark.parametrize('trace,visible', [
    ({'tool_loop': {'status': 'error', 'unavailable_capability': 'tools', 'recorded_results': 0}}, True),
    ({'tool_loop': {'status': 'error', 'error_type': 'TimeoutError', 'recorded_results': 1}}, True),
    ({'tool_loop': {'stop_reason': 'model_stop', 'results': []}}, False),
    ({'status': 'skipped', 'reason': 'scope_none'}, False),
])
def test_tool_failure_notice_renders_without_evidence_and_without_changing_answer(trace, visible):
    from sovushka.components.chat_rendering import _render_evidence_header
    meta = {'retrieval_trace': trace, 'answer': 'Untouched model answer'}
    with Client(page('/__tool_failure_notice')) as client:
        _render_evidence_header(meta, [])
        texts = [str(getattr(element, 'text', '')) for element in client.elements.values()]
        assert ('Не удалось выполнить действие' in texts) == visible
        if visible:
            assert any(element._props.get('role') == 'status' for element in client.elements.values())
        if trace.get('tool_loop', {}).get('unavailable_capability') == 'tools':
            assert any('в этом запросе не выполнены' in text for text in texts)
        if trace.get('tool_loop', {}).get('recorded_results') == 1:
            assert not any('в этом запросе не выполнены' in text for text in texts)
    assert meta['answer'] == 'Untouched model answer'


@pytest.mark.asyncio
async def test_guide_search_clear_and_close_callbacks():
    from sovushka.components.light_guide import open_user_guide

    with Client(page('/__guide_actions')) as client:
        dialog = open_user_guide()
        search = next(e for e in client.elements.values() if e._props.get('label') == 'Найти в руководстве')
        search.set_value('несуществующее-слово-987')
        assert any('Ничего не найдено' in str(getattr(e, 'text', '')) for e in client.elements.values())
        search.set_value('')
        assert not any('Ничего не найдено' in str(getattr(e, 'text', '')) for e in client.elements.values())
        close = next(e for e in client.elements.values() if e._props.get('aria-label') == 'Закрыть руководство')
        await click(close)
        assert not dialog.value


@pytest.mark.asyncio
@pytest.mark.parametrize('save_ok', [True, False])
async def test_watch_save_refresh_and_close_callbacks(monkeypatch, save_ok):
    from sovushka.components import dataset_watch as module
    from nicegui import core

    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())

    calls, messages = [], []
    response = {'watch': {'enabled': False, 'auto_index': False, 'status': 'waiting'}, 'events': []}

    async def get(endpoint):
        calls.append(('GET', endpoint))
        return response

    async def put(endpoint, body):
        calls.append(('PUT', endpoint, body))
        return response if save_ok else None

    monkeypatch.setattr(module, 'api_get', get)
    monkeypatch.setattr(module, 'api_put', put)
    monkeypatch.setattr(module, 'last_api_error_text', lambda fallback: fallback)
    monkeypatch.setattr(ui, 'notify', lambda message, **kwargs: messages.append((message, kwargs)))
    with Client(page('/__watch_actions')) as client:
        await module.open_dataset_watch('dataset #&', 'C:/Folder with spaces')
        button = lambda text: next(e for e in client.elements.values() if getattr(e, 'text', None) == text)
        toggle = next(e for e in client.elements.values() if isinstance(e, ui.switch) and e.text == 'Отслеживать изменения')
        toggle.set_value(True)
        before = len(calls)
        await click(button('Сохранить'))
        assert calls[before] == ('PUT', '/api/rag/datasets/dataset%20%23%26/watch', {
            'path': 'C:/Folder with spaces', 'enabled': True, 'auto_index': False})
        assert messages[-1][1]['type'] == ('positive' if save_ok else 'negative')
        assert len(calls) == before + (2 if save_ok else 1)
        before = len(calls)
        await click(button('Обновить журнал'))
        assert len(calls) == before + 1
        dialog = next(e for e in client.elements.values() if isinstance(e, ui.dialog))
        before = len(calls)
        await click(button('Закрыть'))
        assert not dialog.value and len(calls) == before


@pytest.mark.asyncio
@pytest.mark.parametrize('action', ['cancel', 'close', 'success', 'failure', 'double'])
async def test_dataset_delete_is_explicit_danger_and_reports_real_result(monkeypatch, action):
    from nicegui import core
    from sovushka.components import dataset_delete as module

    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    calls, notifications, refreshes = [], [], []
    finished = asyncio.Event()
    release = asyncio.Event()

    async def remove(route):
        calls.append(route)
        if action == 'double':
            await release.wait()
        return None if action == 'failure' else {'status': 'deleted'}

    async def refresh():
        refreshes.append(True)
        finished.set()

    monkeypatch.setattr(module, 'api_delete', remove)
    monkeypatch.setattr(module, 'last_api_error_text', lambda fallback: fallback)
    monkeypatch.setattr(ui, 'notify', lambda message, **kwargs: notifications.append((message, kwargs)))
    with Client(page('/__dataset_delete')) as client:
        name = "Документы '); alert(1); // 🌲"
        dialog = module.open_dataset_delete({'id': 'ds #&', 'name': name}, on_deleted=refresh)
        assert dialog.value and not calls
        assert any(getattr(e, 'text', None) == name for e in client.elements.values())
        buttons = [e for e in client.elements.values() if isinstance(e, ui.button)]
        confirm = next(e for e in buttons if e.text == 'Удалить датасет')
        cancel = next(e for e in buttons if e.text == 'Отмена')
        close = next(e for e in buttons if e._props.get('aria-label') == 'Закрыть удаление датасета')
        assert 'sov-ui-button--danger' in confirm._classes
        if action in {'cancel', 'close'}:
            await click(cancel if action == 'cancel' else close)
            assert not dialog.value and not calls and not notifications
            return
        await click(confirm)
        if action == 'double':
            assert dialog.value and confirm._props['disable'] and close._props['disable']
            await click(confirm)
            assert len(calls) == 1
            release.set()
            await asyncio.wait_for(finished.wait(), timeout=1)
        assert calls == ['/api/rag/datasets/ds%20%23%26']
        if action == 'failure':
            assert dialog.value and not notifications and not refreshes
            assert not confirm._props.get('disable')
            assert any('Удаление не подтверждено' in str(getattr(e, 'text', '')) for e in client.elements.values())
        else:
            assert not dialog.value and refreshes == [True]
            assert notifications[-1][1]['type'] == 'positive'


@pytest.mark.asyncio
@pytest.mark.parametrize('outcome', ['cancel', 'success', 'failure', 'missing-model', 'missing-name', 'missing-address'])
async def test_connection_editor_fields_save_and_cancel(monkeypatch, outcome):
    from nicegui import core
    from sovushka.pages import model_connections as module

    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    monkeypatch.setattr(module, 'is_light', lambda: True)
    timers, requests, notifications = [], [], []

    async def get(route):
        return {}

    async def post(route, payload):
        requests.append((route, payload))
        return {'connection_id': 'synthetic'} if outcome == 'success' else None

    monkeypatch.setattr(module, 'api_get', get)
    monkeypatch.setattr(module, 'api_post', post)
    monkeypatch.setattr(module, 'last_api_error_text', lambda fallback: fallback)
    monkeypatch.setattr(ui, 'timer', lambda interval, callback, **kwargs: timers.append(callback))
    monkeypatch.setattr(ui, 'notify', lambda message, **kwargs: notifications.append((message, kwargs)))
    with Client(page('/__model_fields')) as client:
        module.build_model_connections()
        await timers[0]()
        button = lambda label: next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == label)
        await click(button('Добавить подключение'))
        fields = {e._props['label']: e for e in client.elements.values() if 'label' in e._props and hasattr(e, 'set_value')}
        for label, value in {'Название': 'Модель «тест» 🌲', 'Адрес сервиса': 'http://127.0.0.1:11434/v1',
                             'Модель': 'synthetic-model:1', 'Расположение': 'loopback',
                             'Размер контекста, токены': 8192, 'Новый ключ': 'synthetic-test-value'}.items():
            fields[label].set_value(value)
            assert fields[label].value == value
        assert fields['Новый ключ']._props['type'] == 'password'
        dialog = next(e for e in client.elements.values() if isinstance(e, ui.dialog))
        missing = {'missing-model': 'Модель', 'missing-name': 'Название', 'missing-address': 'Адрес сервиса'}.get(outcome)
        if missing:
            fields[missing].set_value('   ')
        await click(button('Отмена' if outcome == 'cancel' else 'Сохранить'))
        if missing:
            assert dialog.value and not requests and not notifications
            assert fields[missing]._props['error'] is True
            assert fields[missing]._props['error-message']
            assert fields['Новый ключ'].value == 'synthetic-test-value'
        elif outcome == 'cancel':
            assert not dialog.value and not requests
        else:
            assert requests == [('/api/model-connections', {'display_name': 'Модель «тест» 🌲',
                'base_url': 'http://127.0.0.1:11434/v1', 'model_id': 'synthetic-model:1',
                'locality': 'loopback', 'requested_context_tokens': 8192,
                'extension_type': None, 'secret_value': 'synthetic-test-value'})]
            assert dialog.value == (outcome == 'failure')
            assert notifications[-1][1]['type'] == ('negative' if outcome == 'failure' else 'positive')


@pytest.mark.asyncio
async def test_edit_connection_does_not_offer_a_secret_field_that_is_not_saved(monkeypatch):
    from nicegui import core
    from sovushka.pages import model_connections as module

    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    monkeypatch.setattr(module, 'is_light', lambda: True)
    timers = []
    async def get(route):
        return {'connections': [{'connection_id': 'test', 'revision_id': 'test:r1', 'display_name': 'Test',
                'base_url': 'http://127.0.0.1:11434/v1', 'model_id': 'test', 'locality': 'loopback'}]} if route == '/api/model-connections' else {}
    monkeypatch.setattr(module, 'api_get', get)
    monkeypatch.setattr(ui, 'timer', lambda interval, callback, **kwargs: timers.append(callback))
    with Client(page('/__edit_model')) as client:
        module.build_model_connections()
        await timers[0]()
        edit = next(e for e in client.elements.values() if isinstance(e, ui.menu_item) and
                    any(getattr(child, 'text', None) == 'Изменить' for child in e.default_slot.children))
        await click(edit)
        assert not any(e._props.get('label') == 'Новый ключ' for e in client.elements.values())
        assert any('Ещё → Заменить ключ' in str(getattr(e, 'text', '')) for e in client.elements.values())


@pytest.mark.asyncio
@pytest.mark.parametrize('operation,label', [('_rename_dataset', 'Новое название датасета'), ('_change_group', 'Название группы')])
@pytest.mark.parametrize('outcome', ['cancel', 'failure', 'success'])
async def test_dataset_edit_dialog_preserves_input_on_failure(monkeypatch, operation, label, outcome):
    from nicegui import core
    from sovushka.pages import samovar

    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    calls, refreshed, notifications = [], [], []
    value = 'Новое «имя» #& 🌲'
    async def patch(route):
        calls.append(route)
        return {'name': value} if outcome == 'success' else None
    async def refresh():
        refreshed.append(True)
    monkeypatch.setattr(ui, 'notify', lambda message, **kwargs: notifications.append((message, kwargs)))
    # Execute the actual nested dialog factory without constructing unrelated legacy panels.
    tree = ast.parse(Path(samovar.__file__).read_text(encoding='utf-8'))
    factory = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef) and node.name == operation)
    namespace = {**vars(samovar), 'api_patch': patch, '_refresh': refresh}
    exec(compile(ast.Module(body=[factory], type_ignores=[]), samovar.__file__, 'exec'), namespace)
    with Client(page('/__dataset_edit')) as client:
        namespace[operation]({'id': 'ds #&', 'name': 'Old', 'group': ''})
        field = next(e for e in client.elements.values() if e._props.get('label') == label)
        field.set_value(value)
        dialog = next(e for e in client.elements.values() if isinstance(e, ui.dialog))
        action = next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == ('Отмена' if outcome == 'cancel' else 'Сохранить'))
        await click(action)
        if outcome == 'cancel':
            assert not dialog.value and not calls
        elif outcome == 'failure':
            assert dialog.value and field.value == value and not refreshed
            assert notifications[-1][1]['type'] == 'negative'
        else:
            assert not dialog.value and refreshed == [True]
            assert 'ds%20%23%26' in calls[0] and '%23%26' in calls[0]


@pytest.mark.asyncio
@pytest.mark.parametrize('watch_outcome', ['enabled', 'disabled', 'retry'])
async def test_folder_intake_failure_keeps_dataset_and_retry_reuses_id(monkeypatch, watch_outcome):
    from sovushka.components import folder_setup as samovar

    calls, notifications = [], []
    attempts = 0
    watch_attempts = 0
    async def put(route, body):
        nonlocal watch_attempts
        watch_attempts += 1
        calls.append((route, body))
        assert route == '/api/rag/datasets/created-once/watch'
        assert body == {'path': 'C:/synthetic folder', 'enabled': True, 'auto_index': True}
        return None if watch_outcome == 'retry' and watch_attempts == 1 else {'watch': {'enabled': True}}
    async def post(route, body):
        nonlocal attempts
        calls.append((route, body))
        if route.endswith('/intake-plan'):
            return {'status': 'ok'}
        if route == '/api/rag/datasets':
            assert body == {'name': 'Folder'}
            return {'id': 'created-once'}
        assert route == '/api/rag/index-external'
        attempts += 1
        return None if attempts == 1 else {'status': 'started'}
    async def forbidden_delete(*args, **kwargs):
        pytest.fail('An uncertain intake result must never delete the dataset')
    async def refresh():
        return None
    monkeypatch.setattr(ui, 'notify', lambda message, **kwargs: notifications.append((message, kwargs)))
    tree = ast.parse(Path(samovar.__file__).read_text(encoding='utf-8'))
    factory = next(node for node in ast.walk(tree) if isinstance(node, ast.AsyncFunctionDef) and node.name == '_submit_add')
    with Client(page('/__intake_retry')):
        with ui.dialog() as dialog:
            name, path, parse = ui.input(value='Folder'), ui.input(value='C:/synthetic folder'), ui.switch(value=True)
            watch = ui.switch(value=watch_outcome != 'disabled')
        dialog.open()
        picked = {'path': ''}
        namespace = {**vars(samovar), 'api_post': post, 'api_put': put, 'api_delete': forbidden_delete,
                     '_refresh': refresh, '_add_error': lambda text: notifications.append((text, {'type': 'negative'})) if text else None, 'add_log': lambda text: None,
                     'last_api_error_text': lambda fallback: fallback,
                     'name_in': name, 'path_in': path, 'parse_sw': parse, 'watch_sw': watch, 'picked': picked, 'add_dialog': dialog, 'on_connected': None}
        exec(compile(ast.Module(body=[factory], type_ignores=[]), samovar.__file__, 'exec'), namespace)
        await namespace['_submit_add']()
        assert dialog.value and picked['dataset_id'] == 'created-once'
        assert path.value == 'C:/synthetic folder' and notifications[-1][1]['type'] == 'negative'
        await namespace['_submit_add']()
        if watch_outcome == 'retry':
            assert dialog.value and notifications[-1][1]['type'] == 'negative'
            await namespace['_submit_add']()
        assert watch_attempts == {'enabled': 1, 'disabled': 0, 'retry': 2}[watch_outcome]
        assert attempts == 2  # Retrying watch setup never resubmits the accepted intake.
        assert not dialog.value and notifications[-1][1]['type'] == 'positive'
        assert len([call for call in calls if call[0] == '/api/rag/datasets']) == 1
        assert all(call[1]['dataset_id'] == 'created-once' for call in calls if call[0] == '/api/rag/index-external')


@pytest.mark.asyncio
@pytest.mark.parametrize("outcome", ["ok", "empty", "offline", "rejected"])
async def test_local_setup_requires_explicit_selection_and_reuses_saved_connection(monkeypatch, outcome):
    from nicegui import core
    from sovushka.components import model_setup as module
    monkeypatch.setattr(core, "loop", asyncio.get_running_loop())
    calls=[]; completed=[]
    async def get(route): return {"connections":[], "bindings":{}}
    async def post(route, payload):
        calls.append((route,payload))
        if route.endswith("discover-local"):
            return {"engines": []} if outcome == "offline" else {"engines": [{"id": "ollama", "name": "Ollama", "base_url": "http://127.0.0.1:11434/v1", "models": [] if outcome == "empty" else ["test-model"]}]}
        return {"connection_id":"test", "revision_id":"test:r1"}
    async def put(route,payload):
        calls.append((route,payload))
        return None if outcome=="rejected" else {"connection_revision_id":"test:r1"}
    async def done(): completed.append(True)
    monkeypatch.setattr(module,"api_get",get);monkeypatch.setattr(module,"api_post",post);monkeypatch.setattr(module,"api_put",put)
    with Client(page("/__ollama_setup")) as client:
        module.open_model_setup(done)
        button=lambda label: next(e for e in client.elements.values() if isinstance(e,ui.button) and e.text==label)
        enable=button("Проверить и подключить")
        assert not enable.enabled
        await click(button("Найти на компьютере"))
        assert len(calls)==1 and calls[0][0].endswith("discover-local")
        assert not enable.enabled
        if outcome in {"empty","offline"}: return
        model=next(e for e in client.elements.values() if isinstance(e,ui.select) and e._props.get("label")=="Модель")
        model.set_value("test-model")
        await asyncio.sleep(0)
        assert enable.enabled
        await click(enable)
        for _ in range(5): await asyncio.sleep(0)
        assert calls[-1][0].endswith("/roles/answer")
        if outcome=="ok": assert completed==[True]
        else:
            assert not completed
            await click(enable)
            for _ in range(5): await asyncio.sleep(0)
            assert sum(route=="/api/model-connections" for route,payload in calls)==1
        assert enable.enabled


@pytest.mark.asyncio
async def test_connection_check_shows_busy_and_retains_failure(monkeypatch):
    from nicegui import core
    from sovushka.pages import model_connections as module
    monkeypatch.setattr(core, "loop", asyncio.get_running_loop())
    monkeypatch.setattr(module, "is_light", lambda:True)
    timers=[];calls=[]; gate=asyncio.Event()
    async def get(route):
        return {"connections":[{"connection_id":"test", "revision_id":"test:r1", "display_name":"Test", "model_id":"test", "locality":"loopback"}]} if route=="/api/model-connections" else {}
    async def post(route,payload):
        calls.append(route);await gate.wait();return None
    monkeypatch.setattr(module,"api_get",get);monkeypatch.setattr(module,"api_post",post)
    monkeypatch.setattr(ui,"timer",lambda interval,callback,**kw:timers.append(callback))
    monkeypatch.setattr(ui,"notify",lambda *a,**kw:None)
    with Client(page("/__busy_check")) as client:
        module.build_model_connections();await timers[0]()
        button=next(e for e in client.elements.values() if isinstance(e,ui.button) and e.text=="Проверить")
        await click(button)
        assert not button.enabled and button._props.get("loading")
        assert any("Проверяем подключение" in str(getattr(e,"text","")) for e in client.elements.values())
        gate.set()
        for _ in range(8): await asyncio.sleep(0)
        assert button.enabled and len(calls)==1
        assert any("Сервис не ответил" in str(getattr(e,"text","")) for e in client.elements.values())


@pytest.mark.asyncio
async def test_empty_folder_requires_watch_confirmation_before_creating_dataset(monkeypatch):
    from nicegui import core
    from sovushka.components import folder_setup as module
    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    calls=[]
    async def post(route,body):
        calls.append(route)
        if route.endswith('intake-plan'): return dict(status='ok',accepted_count=0,source_state='empty')
        if route.endswith('/datasets'): return {'id':'empty-test'}
        return {'status':'registered'}
    async def put(route,body):
        calls.append(route); return {'watch':{'enabled':True}}
    async def done(): pass
    monkeypatch.setattr(module,'api_post',post);monkeypatch.setattr(module,'api_put',put)
    monkeypatch.setattr(ui,'notify',lambda *a,**kw:None)
    with Client(page('/__empty_folder')) as client:
        dialog=ui.dialog()
        module.open_folder_setup(dialog,pick_folder=done,on_done=done,initial_path='C:/Empty folder')
        button=next(e for e in client.elements.values() if isinstance(e,ui.button) and e.text=='Подключить папку')
        await click(button)
        for _ in range(5): await asyncio.sleep(0)
        assert calls==['/api/rag/external/intake-plan'] and dialog.value
        assert button.text=='Подключить под наблюдение'
        await click(button)
        for _ in range(5): await asyncio.sleep(0)
        assert not dialog.value
        assert calls.count('/api/rag/datasets')==1 and calls[-1].endswith('/watch')
