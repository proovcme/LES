"""Cross-engine database compatibility and rollback, no models or separate catalog."""
import hashlib
import sqlite3

import pytest

from proxy.services import chat_session_service as sessions, chat_draft_service as drafts


@pytest.fixture
def db(tmp_path,monkeypatch,workspace_engine):
    path=tmp_path/'Диалог 日本語.sqlite'
    monkeypatch.setattr(sessions,'rag_meta_db_path',lambda:str(path))
    return path


def test_reading_other_engine_data_preserves_every_byte(db,monkeypatch):
    from tests.oracles import chat_session_python as previous_sessions, chat_draft_python as previous_drafts
    monkeypatch.setattr(previous_sessions,'rag_meta_db_path',lambda:str(db))
    session=previous_sessions.create_session(title='Исходный диалог 🌲')
    sid=session['session_id']
    content=previous_drafts.save_draft(sid,0,'  Первая строка\n日本語\t🌲  ',{'attachment_id':'read_123456789abc','name':'  Таблица #1.xlsx'})
    digest=hashlib.sha256(db.read_bytes()).hexdigest()
    assert sessions.get_session(sid)==session
    assert drafts.get_draft(sid)==content
    assert hashlib.sha256(db.read_bytes()).hexdigest()==digest


def test_failed_sqlite_write_rolls_back_session_and_related_effects(db):
    session=sessions.create_session(title='Сохранить')
    with sqlite3.connect(db) as conn:
        conn.executescript('''CREATE TABLE proof(value TEXT);
            CREATE TRIGGER reject_title AFTER UPDATE ON les_chat_sessions BEGIN
            INSERT INTO proof VALUES('must roll back'); SELECT RAISE(ABORT,'fixture refusal'); END;''')
    with pytest.raises(sqlite3.DatabaseError):
        sessions.update_session(session['session_id'],title='Потерять')
    assert sessions.get_session(session['session_id'])==session
    with sqlite3.connect(db) as conn:
        assert conn.execute('SELECT count(*) FROM proof').fetchone()[0]==0


def test_unicode_whitespace_and_timestamp_offsets_keep_previous_behavior(db):
    session=sessions.create_session(title='\x1f Название 日本語 \x1c')
    assert session['title']=='Название 日本語'
    sid=session['session_id']
    with sqlite3.connect(db) as conn:
        conn.execute('UPDATE les_chat_sessions SET updated_at=?',('2026-11-01T12:00:00+03:00',))
        conn.execute('CREATE TABLE chat_history (id INTEGER PRIMARY KEY,session_id TEXT,timestamp TEXT,question TEXT)')
        conn.execute('INSERT INTO chat_history VALUES(1,?,?,?)',('legacy','2026-11-01 09:00:00.000001','Later by one microsecond'))
    assert [row['session_id'] for row in sessions.list_sessions()]==['legacy',sid]
    with pytest.raises(ValueError):sessions.update_session(sid,role='\x1f')


def test_every_invalid_scope_leaves_previous_record_intact(db):
    session=sessions.create_session()
    for scope in [{'project_ids':[True]}, {'project_ids':[1.0]}, {'dataset_ids':['']},
                  {'selected_sources_only':1}, {'unknown':True}, {'scope_type':'none','project_ids':[1]}]:
        with pytest.raises(ValueError):sessions.update_session(session['session_id'],scope=scope)
        assert sessions.get_session(session['session_id'])==session


def test_missing_native_owner_is_an_error_and_never_creates_an_empty_catalog(tmp_path,monkeypatch):
    path=tmp_path/'missing.sqlite'
    monkeypatch.setattr(sessions,'rag_meta_db_path',lambda:str(path))
    monkeypatch.setenv('LES_CORE_LIBRARY',str(tmp_path/'absent.dll'))
    with pytest.raises(sqlite3.OperationalError,match='Восстановите установку'):
        sessions.list_sessions()
    assert not path.exists()
