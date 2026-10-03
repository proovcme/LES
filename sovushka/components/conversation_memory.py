"""One inspectable memory panel: automatic compaction, edit and forget."""
from urllib.parse import quote
from nicegui import ui
from sovushka.state import api_get, api_patch, api_post
from sovushka.uikit import action_button, checkbox_field, text_field
from sovushka.components.activity import ActivityPanel


async def build_conversation_memory(session_id: str):
    endpoint = f'/api/workspace/memory/context/{quote(str(session_id or ""), safe="")}'
    state = await api_get(endpoint)
    if not isinstance(state, dict) or 'revision' not in state:
        ui.label('Память разговора пока недоступна. Сначала создайте или откройте чат.').classes('sov-muted')
        return
    ui.label('Этот разговор').classes('sov-panel-title')
    status = ui.label('').classes('sov-ui-section-detail').props('role="status" aria-live="polite"')
    enabled = checkbox_field('Помнить этот разговор', value=state['enabled'])
    auto = checkbox_field('Автоматически сжимать длинный разговор', value=state['auto_summary'])
    ui.label('Сводка сохраняет ход работы и дополняет последние сообщения. Полная история остаётся доступной. Используется выбранная модель чата.').classes('sov-ui-section-detail')
    summary = text_field(label='Что ЛЕС помнит о разговоре', value=state['summary'], classes='w-full').props('type=textarea autogrow maxlength=2200')
    ui.label('Сводка может ошибаться: её можно исправить. Доказательства берутся из документов, а не из памяти.').classes('sov-ui-section-detail')
    activity = ActivityPanel('Память готова')
    activity.finish('Память готова')
    activity.root.set_visibility(False)

    def display_counts():
        error = state.get('last_error')
        status.set_text(error or f"Сообщений с ответами в памяти: {state.get('remembered_turns', 0)}. В сводке: {state.get('summarized_turns', 0)}.")

    async def perform(action):
        for button in controls: button.disable()
        status.set_text('Сохраняем…' if action == 'save' else 'Обновляем сводку…' if action == 'summarize' else 'Забываем прежний контекст…')
        activity.start(status.text)
        try:
            body = {'expected_revision': state['revision']}
            if action == 'save':
                body.update(summary=str(summary.value or ''), enabled=bool(enabled.value), auto_summary=bool(auto.value))
                result = await api_patch(endpoint, body)
            else:
                result = await api_post(endpoint + '/' + action, body)
            if not isinstance(result, dict):
                activity.finish('Изменения не подтверждены')
                status.set_text('Изменения не подтверждены. Ваш текст сохранён в поле; обновите память перед повтором.')
                return
            fresh = await api_get(endpoint)
            state.update(fresh if isinstance(fresh, dict) else result)
            summary.set_value(state['summary'])
            enabled.set_value(state['enabled']); auto.set_value(state['auto_summary'])
            display_counts()
            activity.finish('Память обновлена')
        finally:
            if not activity.finished:
                activity.finish('Операция не завершена')
            for button in controls: button.enable()

    async def refresh():
        fresh = await api_get(endpoint)
        if isinstance(fresh, dict):
            state.update(fresh)
            # Keep unsaved edits; only revision and counts are refreshed.
            display_counts()
        else:
            status.set_text('Не удалось обновить память. Введённый текст сохранён.')

    with ui.row().classes('w-full gap-2'):
        save = action_button('Сохранить память', on_click=lambda: perform('save'), variant='primary')
        compress = action_button('Сжать сейчас', on_click=lambda: perform('summarize'), variant='secondary')
        reload = action_button('Обновить состояние', on_click=refresh, variant='quiet')
    with ui.expansion('Забыть прежний разговор').classes('w-full'):
        ui.label('Сводка и прежние сообщения перестанут участвовать в новых ответах. История чата и записи памяти проекта останутся. Новые сообщения можно будет запоминать снова.')
        forget = action_button('Забыть контекст', on_click=lambda: perform('forget'), variant='danger')
    controls = (save, compress, reload, forget, enabled, auto, summary)
    display_counts()
