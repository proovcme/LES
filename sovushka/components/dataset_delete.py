"""Explicit destructive action; original external folders are never the target."""
from urllib.parse import quote

from nicegui import ui

from sovushka.state import api_delete, last_api_error_text
from sovushka.uikit.components import action_button, panel, section_heading, status_badge


def open_dataset_delete(dataset, *, on_deleted, after_delete_description=''):
    name = str(dataset.get('name') or dataset.get('id') or '')
    dataset_id = str(dataset.get('id') or '')
    if not dataset_id:
        ui.notify('Датасет не выбран. Обновите список и повторите действие.', type='negative')
        return None
    busy = False
    with ui.dialog() as dialog, panel(variant='raised', classes='sov-ui-dialog p-4'):
        with ui.row().classes('w-full items-center justify-between'):
            status_badge('Опасное действие', 'error')
            close = action_button(icon='o_close', icon_only=True, aria_label='Закрыть удаление датасета',
                                  variant='quiet', on_click=dialog.close)
        section_heading('Удалить датасет?')
        ui.label(name).classes('w-full break-all font-semibold')
        ui.label('Датасет, его документы и поисковый индекс исчезнут из ЛЕС. Файлы подключённой внешней папки останутся на месте.').classes('w-full')
        ui.label('Перед удалением создаётся копия для восстановления. При отказе операции сообщение об успехе не появится.').classes('w-full')
        if after_delete_description:
            ui.label(after_delete_description).classes('w-full')
        feedback = ui.label('').classes('w-full break-all')

        async def remove():
            nonlocal busy
            if busy:
                return
            busy = True
            for button in (confirm, cancel, close):
                button.disable()
            dialog.props('persistent')
            feedback.set_text('Удаляем датасет…')
            try:
                result = await api_delete(f'/api/rag/datasets/{quote(dataset_id, safe="")}')
                if not isinstance(result, dict) or result.get('status') != 'deleted':
                    feedback.set_text(last_api_error_text('Удаление не подтверждено. Обновите список и проверьте состояние датасета перед повторной попыткой.'))
                    return
                dialog.close()
                ui.notify(f'Датасет удалён: {name}', type='positive')
                await on_deleted()
            finally:
                busy = False
                for button in (confirm, cancel, close):
                    button.enable()
                dialog.props(remove='persistent')

        with ui.row().classes('w-full justify-end gap-2'):
            cancel = action_button('Отмена', on_click=dialog.close, variant='quiet')
            confirm = action_button('Удалить датасет', on_click=remove, variant='danger')
    dialog.open()
    return dialog
