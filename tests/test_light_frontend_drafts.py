import sqlite3
from concurrent.futures import ThreadPoolExecutor

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
import pytest

from proxy.routers import workspace
from proxy.services import chat_draft_service as drafts, chat_session_service as sessions


@pytest.fixture
def client(tmp_path, monkeypatch):
    path = tmp_path / 'catalog.sqlite'
    monkeypatch.setattr(sessions, 'rag_meta_db_path', lambda: str(path))
    app = FastAPI()
    app.include_router(workspace.router)
    app.dependency_overrides[workspace.require_user] = lambda: object()
    with TestClient(app) as client:
        yield client


def test_draft_roundtrip_cas_and_scope_unchanged(client):
    session = client.post('/api/workspace/sessions', json={'title': 'Проверка'}).json()
    url = f"/api/workspace/sessions/{session['session_id']}/draft"
    assert client.get(url).json() == dict(revision=0, text='', attachment=None)
    content = dict(revision=0, text='Кириллица, пробелы и 🌲\nВторая строка', attachment=dict(attachment_id='read_123456789abc', name='Тест с пробелами.xlsx'))
    saved = client.put(url, json=content)
    assert saved.status_code == 200
    assert saved.json() == content | dict(revision=1)
    assert client.put(url, json=content | dict(text='Запоздалый')).status_code == 409
    assert client.get(url).json()['text'] == content['text']
    assert client.get(f"/api/workspace/sessions/{session['session_id']}").json()['scope'] == session['scope']
    assert client.put(url, json=dict(revision=1, text='', attachment=None)).json()['revision'] == 2
    assert drafts.get_draft(session['session_id'])['text'] == ''  # fresh SQLite connection, no UI memory


def test_draft_validation_and_authentication(client):
    sid = client.post('/api/workspace/sessions', json={}).json()['session_id']
    url = f'/api/workspace/sessions/{sid}/draft'
    for body in [dict(revision=-1, text='bad'), dict(revision=0, text='x' * 20001),
                 dict(revision=0, text='x', attachment={'attachment_id': '../secret', 'name': 'X'}),
                 dict(revision=0, text='x', source_path='private')]:
        assert client.put(url, json=body).status_code == 422
    assert client.get('/api/workspace/sessions/missing/draft').status_code == 404
    assert client.put('/api/workspace/sessions/missing/draft', json=dict(revision=0, text='x')).status_code == 404
    def deny():
        raise HTTPException(401, 'Unauthorized')
    client.app.dependency_overrides[workspace.require_user] = deny
    assert client.get(url).status_code == 401
    assert client.put(url, json=dict(revision=0, text='secret')).status_code == 401


def test_two_windows_only_one_cas_write_wins(client):
    sid = sessions.create_session()['session_id']
    def save(text):
        try:
            return drafts.save_draft(sid, 0, text)['text']
        except sessions.SessionConflictError:
            return 'conflict'
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(save, ['one', 'two']))
    assert results.count('conflict') == 1
    assert drafts.get_draft(sid)['text'] in {'one', 'two'}


def test_read_absent_database_never_creates_it(tmp_path, monkeypatch):
    path = tmp_path / 'absent.sqlite'
    monkeypatch.setattr(sessions, 'rag_meta_db_path', lambda: str(path))
    with pytest.raises(LookupError):
        drafts.get_draft('missing')
    assert not path.exists()


def test_busy_draft_returns_an_actionable_error_without_overwriting(client, monkeypatch):
    session = client.post('/api/workspace/sessions', json={}).json()
    path = f"/api/workspace/sessions/{session['session_id']}/draft"
    before = client.get(path).json()
    def busy(*args, **kwargs):
        raise sqlite3.OperationalError('database is locked')
    monkeypatch.setattr(drafts, 'save_draft', busy)
    response = client.put(path, json=dict(revision=0, text='Keep my draft'))
    assert response.status_code == 503 and 'Текст остаётся в этом окне' in response.json()['detail']
    assert client.get(path).json() == before


def test_history_failure_is_an_error_not_an_empty_conversation(tmp_path, monkeypatch):
    from proxy.routers import chat_history
    monkeypatch.setattr(chat_history, 'rag_meta_db_path', lambda: str(tmp_path))  # a directory cannot be SQLite
    app = FastAPI()
    app.include_router(chat_history.router)
    app.dependency_overrides[chat_history.require_user] = lambda: object()
    with TestClient(app) as client:
        result = client.get('/api/chat/history?session_id=known')
        assert result.status_code == 503
        assert 'Переписка не удалена' in result.json()['detail']
