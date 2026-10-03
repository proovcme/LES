"""Navigation behavior, preserved drafts and measurable progress, without Computer Use."""
import shutil
import subprocess
from pathlib import Path
from types import SimpleNamespace

from nicegui import Client, ui
from nicegui.page import page

from sovushka.components.chat_drafts import ChatDrafts
from sovushka.components.navigation import bind_route_tabs, return_controls
from sovushka.components.activity import ActivityPanel


def test_actual_javascript_navigation():
    node = shutil.which('node')
    assert node
    subprocess.run([node, 'tests/navigation_controls.cjs'], check=True, capture_output=True, text=True)


def test_drafts_survive_switch_and_reconstruction_without_crossing_sessions():
    storage = {}
    draft = ChatDrafts(storage, 'a')
    draft.save('Задача Café #1\nПрочитать папку')
    assert draft.switch('b', draft.read()) == ''
    draft.save('Другой проект')
    assert draft.switch('a', draft.read()) == 'Задача Café #1\nПрочитать папку'
    assert ChatDrafts(storage, 'b').read() == 'Другой проект'
    draft.save('')
    assert ChatDrafts(storage, 'a').read() == ''
    for index in range(30):
        draft.switch(str(index), 'x' * 25000)
    assert len(storage[draft.KEY]) <= 16
    assert max(map(len, storage[draft.KEY].values())) <= 20000


def test_tab_back_uses_existing_widgets_and_returns_controls(monkeypatch):
    scripts, events = [], {}
    monkeypatch.setattr(ui, 'run_javascript', lambda script: scripts.append(script))
    monkeypatch.setattr(ui, 'on', lambda name, handler: events.update({name:handler}))
    with Client(page('/__nav_contract')) as client:
        return_controls()
        with ui.tabs() as tabs:
            refs = {'chat': ui.tab('Чат'), 'data': ui.tab('Данные')}
        bind_route_tabs(tabs, refs, '/classic')
        tabs.set_value(refs['data'])
        before = set(client.elements)
        events['les-navigation'](SimpleNamespace(args={'path':'/classic','tab':'chat'}))
        assert tabs.value is refs['chat']
        assert before == set(client.elements)
        assert any('/classic?tab=data' in script for script in scripts)
        assert any(isinstance(el,ui.button) and el.props.get('data-les-back') is True for el in client.elements.values())


def test_progress_never_invents_a_percentage_and_keeps_terminal_status():
    with Client(page('/__activity_contract')):
        panel = ActivityPanel('Подготовка')
        assert panel.bar.props.get('indeterminate') is True
        panel.update({'label':'Файлы', 'completed':2, 'total':8})
        assert panel.bar.value == .25
        assert not panel.bar.props.get('indeterminate')
        panel.update({'label':'Ответ модели'})
        assert panel.bar.props.get('indeterminate') is True
        panel.finish('Остановлено')
        panel.update({'label':'Готово'})
        assert panel.status.text == 'Остановлено'
        assert not panel.bar.visible


def test_shared_navigation_script_is_in_both_shells_and_package():
    assert 'navigation.js' in Path('sovushka_ng.py').read_text(encoding='utf-8')
    assert 'navigation.js' in Path('qdrant_visualizer/index.html').read_text(encoding='utf-8')
    assert 'qdrant_visualizer/navigation.js' in Path('tools/build_light_package.py').read_text(encoding='utf-8')
