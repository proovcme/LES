"""Folder selection must not register documents until the user confirms."""
import asyncio
import ctypes as ct
import os

import pytest
from nicegui import Client, core, ui
from nicegui.page import page


@pytest.mark.asyncio
@pytest.mark.parametrize('outcome', ['selected', 'cancelled', 'failed'])
async def test_dataset_browse_uses_native_picker_and_preserves_draft(monkeypatch, outcome):
    from sovushka.components.folder_setup import open_folder_setup
    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    calls = []
    async def pick(**kwargs):
        calls.append(kwargs)
        if outcome == 'failed':
            raise OSError('unavailable')
        return 'C:/Документы/Проект № 2' if outcome == 'selected' else ''
    async def done():
        pytest.fail('Browsing must not connect a dataset')
    with Client(page('/__native_folder')) as client:
        dialog = ui.dialog()
        open_folder_setup(dialog, pick_folder=pick, on_done=done, initial_path='C:/Старая папка')
        fields = {e._props.get('label'): e for e in client.elements.values() if isinstance(e, ui.input)}
        fields['Название'].set_value('Моё название')
        button = next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == 'Выбрать папку')
        handler = next(e.handler for e in button._event_listeners.values() if e.type == 'click')
        await handler()
        assert calls == [{'initial': 'C:/Старая папка', 'title': 'Выберите папку с документами'}]
        assert fields['Название'].value == 'Моё название'
        assert fields['Папка с документами'].value == ('C:/Документы/Проект № 2' if outcome == 'selected' else 'C:/Старая папка')
        assert dialog.value and button.enabled and not button._props.get('loading')
        assert len([e for e in client.elements.values() if isinstance(e, ui.dialog)]) == 1
        if outcome == 'failed':
            assert any('Вставьте путь из Проводника' in str(getattr(e, 'text', '')) for e in client.elements.values())


@pytest.mark.asyncio
async def test_duplicate_click_does_not_open_second_picker(monkeypatch):
    from sovushka.components.folder_setup import open_folder_setup
    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    entered, release = asyncio.Event(), asyncio.Event()
    calls = []
    async def pick(**kwargs):
        calls.append(kwargs)
        entered.set()
        await release.wait()
        return 'C:/Документы'
    async def done(): pass
    with Client(page('/__native_folder_busy')) as client:
        dialog = ui.dialog()
        open_folder_setup(dialog, pick_folder=pick, on_done=done)
        button = next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == 'Выбрать папку')
        handler = next(e.handler for e in button._event_listeners.values() if e.type == 'click')
        task = asyncio.create_task(handler())
        await entered.wait()
        try:
            assert not button.enabled and button._props['loading']
            await handler()
            assert len(calls) == 1
        finally:
            release.set()
            await task
        assert button.enabled


def test_picker_busy_and_failure_release_lock(monkeypatch):
    from sovushka import windows_folder_picker as module
    with module._picker_lock:
        assert module.pick_folder(initial='', title='Folder')['status'] == 'busy'
    def fail(**kwargs):
        raise OSError('Windows failed')
    monkeypatch.setattr(module, '_pick_folder', fail)
    with pytest.raises(OSError):
        module.pick_folder(initial='', title='Folder')
    assert not module._picker_lock.locked()


@pytest.mark.skipif(os.name != 'nt', reason='Windows COM integration')
def test_real_windows_dialog_keeps_unicode_folder_and_options(tmp_path):
    from sovushka.windows_folder_picker import _folder_dialog, _method, _release, _check
    folder = tmp_path / 'Папка № 2 с пробелами'
    folder.mkdir()
    with _folder_dialog(str(folder), 'Документы') as (dialog, ole):
        options = ct.c_ulong()
        _check(_method(dialog, 10, ct.POINTER(ct.c_ulong))(dialog, ct.byref(options)))
        assert options.value & 0x868 == 0x868
        item, path = ct.c_void_p(), ct.c_void_p()
        try:
            _check(_method(dialog, 13, ct.POINTER(ct.c_void_p))(dialog, ct.byref(item)))
            _check(_method(item, 5, ct.c_ulong, ct.POINTER(ct.c_void_p))(item, 0x80058000, ct.byref(path)))
            assert ct.wstring_at(path) == str(folder)
        finally:
            if path:
                ole.CoTaskMemFree(path)
            _release(item)


@pytest.mark.skipif(os.name != 'nt', reason='Windows COM integration')
def test_windows_cancel_is_not_an_error(monkeypatch, tmp_path):
    from sovushka import windows_folder_picker as module
    original = module._method
    def method(pointer, index, *types):
        # Do not show or automate a native window in the test suite.
        if index == 3:
            return lambda *_: -2147023673  # HRESULT_FROM_WIN32(ERROR_CANCELLED)
        return original(pointer, index, *types)
    monkeypatch.setattr(module, '_method', method)
    assert module.pick_folder(initial=str(tmp_path), title='Документы') == {'status': 'cancelled', 'path': ''}
    assert not module._picker_lock.locked()
