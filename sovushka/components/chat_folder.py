"""Two explicit folder actions from chat, sharing the normal dataset workflow."""
from urllib.parse import urlencode
from nicegui import ui
from sovushka.config import UI_PORT
from sovushka.state import api_get, api_post, last_api_error_text, refresh_samovar
from sovushka.uikit import panel, section_heading, text_field, action_button
from sovushka.components.folder_setup import open_folder_setup


def open_chat_folder(on_attached):
    async def pick_folder(*, initial='', title='Выберите папку'):
        result = await api_get('/lite-runtime/pick-folder?' + urlencode(dict(initial=initial, title=title)),
                               base=f'http://127.0.0.1:{UI_PORT}')
        if not isinstance(result, dict) or result.get('status') not in {'selected', 'cancelled'}:
            ui.notify('Не удалось открыть выбор папки. Вставьте путь из Проводника.', type='warning')
        return str(result.get('path') or '') if isinstance(result, dict) and result.get('status') == 'selected' else ''

    with ui.dialog() as dialog, panel(classes='sov-ui-dialog'):
        section_heading('Папка для чата', 'ЛЕС только читает исходники. Выберите, как использовать документы.')
        path = text_field(label='Папка', placeholder='Выберите папку или вставьте путь', classes='w-full')
        async def browse():
            if browsing['active']:
                return
            browsing['active'] = True
            browse_button.disable()
            browse_button.props('loading')
            try:
                selected = await pick_folder(initial=path.value)
                if selected:
                    path.set_value(selected)
            finally:
                browsing['active'] = False
                browse_button.enable()
                browse_button.props(remove='loading')
        browsing = {'active': False}
        browse_button = action_button('Выбрать папку', icon='o_folder_open', on_click=browse, variant='secondary')
        error = ui.label('').props('role=alert').classes('sov-folder-connect-error')
        async def read():
            if not path.value.strip():
                error.set_text('Сначала выберите папку.')
                return
            read_button.disable(); index_button.disable(); path.disable()
            error.set_text('Читаю документы…')
            try:
                result = await api_post('/api/rag/attach-folder', {'path': path.value})
                if not isinstance(result, dict):
                    error.set_text(last_api_error_text('Не удалось прочитать папку.'))
                    return
                await on_attached(result)
                dialog.close()
            finally:
                read_button.enable(); index_button.enable(); path.enable()
        async def index():
            dialog.close()
            add_dialog = ui.dialog()
            open_folder_setup(add_dialog, pick_folder=pick_folder, on_done=refresh_samovar,
                              initial_path=path.value, on_connected=on_attached)
        ui.label('Для этой задачи — до 25 документов, без постоянного индекса. Для больших папок используйте базу знаний.').classes('sov-ui-section-detail')
        read_button = action_button('Прочитать для этой задачи', on_click=read, variant='primary')
        index_button = action_button('Добавить в базу знаний', on_click=index, variant='secondary')
        action_button('Отмена', on_click=dialog.close, variant='quiet')
    dialog.open()
    return dialog
