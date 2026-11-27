"""Rust workspace compatibility with previously written SQLite databases."""
import hashlib
import json
import sqlite3

import pytest

from backend.workspace_native import call
from proxy.services import chat_session_service as sessions


@pytest.fixture
def db(tmp_path,monkeypatch,workspace_engine):
    path=tmp_path/'Диалог 日本語.sqlite'
    monkeypatch.setattr(sessions,'rag_meta_db_path',lambda:str(path))
    return path


def test_reading_saved_session_and_draft_preserves_every_byte(db):
    # Rows written with the previous schema before the native owner opens the file.
    sid = 'saved-session'
    stamp = '2026-10-04T10:17:22+03:00'
    scope = {'scope_type': 'datasets', 'project_ids': [], 'dataset_ids': ['existing'], 'selected_sources_only': True}
    content = {
        'text': '  Первая строка\n日本語\t🌲  ',
        'attachment': {'attachment_id': 'read_123456789abc', 'name': '  Таблица #1.xlsx'},
    }
    with sqlite3.connect(db) as conn:
        conn.executescript("""
            CREATE TABLE les_chat_sessions (
                session_id TEXT PRIMARY KEY, project_id INTEGER, title TEXT NOT NULL, role TEXT NOT NULL,
                scope_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
            CREATE INDEX idx_chat_sessions_project ON les_chat_sessions(project_id, updated_at);
            CREATE TABLE les_chat_drafts (
                session_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
                content_json TEXT NOT NULL, updated_at TEXT NOT NULL);
        """)
        conn.execute('INSERT INTO les_chat_sessions VALUES (?, ?, ?, ?, ?, ?, ?)',
                     (sid, 7, 'Исходный диалог 🌲', 'research', json.dumps(scope), stamp, stamp))
        conn.execute('INSERT INTO les_chat_drafts VALUES (?, ?, ?, ?)',
                     (sid, 3, json.dumps(content, ensure_ascii=False), stamp))

    digest = hashlib.sha256(db.read_bytes()).hexdigest()
    assert sessions.get_session(sid) == {
        'session_id': sid, 'project_id': 7, 'title': 'Исходный диалог 🌲', 'role': 'research',
        'scope': scope, 'created_at': stamp, 'updated_at': stamp, 'registered': True,
    }
    assert call(db, 'draft_get', session_id=sid) == {'revision': 3, **content}
    assert hashlib.sha256(db.read_bytes()).hexdigest() == digest


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
