"""Project identity/scope, read integrity and transaction rollback across implementations."""
import hashlib
import sqlite3
from concurrent.futures import ThreadPoolExecutor
import pytest
from proxy.services import project_service as projects, memory_service as memory, conversation_context_service as context

@pytest.fixture
def db(tmp_path,monkeypatch,catalog_engine):
    path=tmp_path/'Проект 日本語 #1.sqlite'
    for module in (projects,memory,context):
        monkeypatch.setattr(module,'rag_meta_db_path',lambda:str(path))
    return path

def test_project_order_links_status_and_delete_preserve_other_tables(db):
    one=projects.create_project('  Корпус 🌲 ',' ШИФР ', ' Адрес ')
    two=projects.create_project('日本語')
    assert (one['name'],one['code'],one['address'])==('Корпус 🌲','ШИФР','Адрес')
    link=projects.link_entity(one['id'],'dataset',' набор #1 ')
    assert projects.link_entity(one['id'],'dataset','набор #1')==link
    projects.link_entity(two['id'],'dataset','набор #1')
    assert projects.project_for_dataset('набор #1')==one['id']
    assert projects.project_dataset_ids(one['id'])==['набор #1']
    projects.set_project_status(two['id'],'archived')
    assert [p['id'] for p in projects.list_projects()]==[one['id'],two['id']]
    with pytest.raises(ValueError):projects.link_entity(999,'dataset','x')
    with pytest.raises(ValueError):projects.set_project_status(one['id'],'unknown')
    with sqlite3.connect(db) as conn:
        conn.execute('CREATE TABLE sentinel(value TEXT)');conn.execute("INSERT INTO sentinel VALUES ('keep')")
    assert projects.delete_project(one['id'])
    assert projects.get_project(one['id']) is None and projects.list_links(one['id'])==[]
    assert projects.project_for_dataset('набор #1')==two['id']
    with sqlite3.connect(db) as conn:assert conn.execute('SELECT value FROM sentinel').fetchone()[0]=='keep'

def test_project_failed_delete_rolls_back_links(db):
    p=projects.create_project('Сохранить');link=projects.link_entity(p['id'],'folder','readonly')
    with sqlite3.connect(db) as conn:
        conn.execute("CREATE TRIGGER refuse_delete BEFORE DELETE ON les_projects BEGIN SELECT RAISE(ABORT,'fixture'); END")
    with pytest.raises(sqlite3.DatabaseError):projects.delete_project(p['id'])
    assert projects.get_project(p['id'])['links']==[link]

def test_read_old_context_and_note_projection_never_mutates_db(db,monkeypatch):
    from tests.oracles import memory_python,context_python
    monkeypatch.setattr(memory_python,'rag_meta_db_path',lambda:str(db))
    monkeypatch.setattr(context_python,'rag_meta_db_path',lambda:str(db))
    note=memory_python.create_note('  Сохранить пробелы 🌲  ',project_id=2,source_session_id='source')
    state=context_python.update_context('source',expected_revision=0,summary='日本語',through_id=7)
    before=hashlib.sha256(db.read_bytes()).hexdigest()
    assert context.get_context('source')==state
    assert memory.project_note_items(project_id=2)[0]['text']==note['text']
    assert hashlib.sha256(db.read_bytes()).hexdigest()==before

def test_concurrent_context_edits_have_one_winner(db):
    def write(index):
        try:context.update_context('same',expected_revision=0,summary=f'window {index}');return 'saved'
        except context.ContextConflict:return 'conflict'
    with ThreadPoolExecutor(max_workers=12) as pool:results=list(pool.map(write,range(12)))
    assert results.count('saved')==1 and results.count('conflict')==11
    assert context.get_context('same')['revision']==1

def test_note_update_failure_rolls_back_text_adoption_and_state(db):
    note=memory.create_note('automatic',project_id=1,auto=True)
    before=memory.list_notes()[0]
    with sqlite3.connect(db) as conn:
        conn.execute("CREATE TRIGGER refuse_update BEFORE UPDATE ON les_notes BEGIN SELECT RAISE(ABORT,'fixture'); END")
    with pytest.raises(sqlite3.DatabaseError):memory.update_note(note['id'],project_id=1,text='explicit',enabled=False)
    assert memory.list_notes()[0]==before

def test_note_creation_preserves_document_edges(db,monkeypatch):
    from proxy.services import edge_service
    monkeypatch.setattr(edge_service,'rag_meta_db_path',lambda:str(db))
    note=memory.create_note('Проверить СП 7.13130 и [[Проект 日本語]]')
    edges=edge_service.edges_for('note',str(note['id']))['out']
    assert any(edge['dst_id']=='СП 7.13130' for edge in edges)
    assert any(edge['dst_id']=='Проект 日本語' for edge in edges)
