"""Real Rust memory/files plus session-bound model tools and independent undo."""
from concurrent.futures import ThreadPoolExecutor
import os
import pytest
from proxy.services import agent_state_service as state, chat_session_service as sessions, project_service as projects

@pytest.fixture
def root(tmp_path,monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build the native agent-files owner')
    root=tmp_path/'Данные Леса 日本語';(root/'data').mkdir(parents=True)
    db=root/'data/meta.db'
    monkeypatch.setenv('LES_PRODUCT_EDITION','light');monkeypatch.setenv('LES_STATE_ROOT',str(root))
    for module in (state,sessions,projects):monkeypatch.setattr(module,'rag_meta_db_path',lambda:str(db))
    return root

def test_user_global_project_memory_scope_and_plain_file_edit(root):
    one=projects.create_project('One')['id'];two=projects.create_project('Two')['id']
    state.call('save',area='user',expected_revision='missing',text='Говори по-русски 🌲')
    state.call('save',area='memory',expected_revision='missing',text='Общее')
    state.call('save',project_id=one,area='memory',expected_revision='missing',text='Только One')
    assert [d['text'] for d in state.call('bundle',project_id=one)['documents']]==['Говори по-русски 🌲','Общее','Только One']
    assert [d['text'] for d in state.call('bundle',project_id=two)['documents']]==['Говори по-русски 🌲','Общее','']
    path=root/'agent/USER.md';path.write_text('Внешняя правка 日本語',encoding='utf-8-sig')
    read=state.call('read',area='user');assert read['text']=='Внешняя правка 日本語'
    changed=state.call('save',area='user',expected_revision=read['revision'],text='New')
    state.call('restore',area='user',expected_revision=changed['revision'],history_id=changed['history_id'],side='before')
    assert path.read_bytes().startswith(b'\xef\xbb\xbf')

def test_concurrent_writers_keep_one_commit_and_original_sources(root,tmp_path):
    source=tmp_path/'Source.txt';source.write_text('Source stays read-only',encoding='utf-8');before=source.read_bytes()
    def write(index):
        try:return state.call('save',area='files',path='notes/План #1.md',expected_revision='missing',text=f'writer {index}')['persisted']
        except state.AgentStateError as error:return error.code
    with ThreadPoolExecutor(max_workers=16) as pool:results=list(pool.map(write,range(16)))
    assert results.count(True)==1 and results.count('AGENT_CONFLICT')==15
    assert len(state.call('history',area='files',path='notes/План #1.md')['items'])==1
    for path in ['../Source.txt',str(source),'a:secret','CON.txt']:
        with pytest.raises(state.AgentStateError):state.call('save',area='files',path=path,expected_revision='missing',text='overwrite')
    assert source.read_bytes()==before
