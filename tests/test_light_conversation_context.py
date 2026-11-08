import asyncio
import sqlite3
from types import SimpleNamespace

import pytest
from proxy.services import conversation_context_service as service
from proxy.services import chat_session_service, memory_service


def test_extractive_summary_cannot_rewrite_plans_dates_or_languages():
    records = ['Пользователь: Планируем завершить в июне 2033. 日本語.', 'ЛЕС: Обсудим.']
    assert service.selected_summary('{"keep":[0]}', records) == records[0]
    for invalid in ('{"keep":["завершено"]}', '{"keep":[true]}', '{"keep":[5]}', '{"keep":[]}', 'Завершено в июне 2033'):
        with pytest.raises(ValueError): service.selected_summary(invalid, records)
    with pytest.raises(ValueError): service.selected_summary('{"keep":[0]}', ['x' * 2201])


@pytest.fixture
def seeded_conversation(tmp_path, monkeypatch):
    path = tmp_path / 'meta.db'
    for module in (service, chat_session_service, memory_service):
        monkeypatch.setattr(module, 'rag_meta_db_path', lambda: str(path))
    sid = chat_session_service.create_session(title='Контекст')['session_id']
    with sqlite3.connect(path) as conn:
        conn.execute('CREATE TABLE chat_history (id INTEGER PRIMARY KEY, session_id TEXT, question TEXT, answer TEXT, success INTEGER, retrieval_trace_json TEXT)')
        conn.executemany('INSERT INTO chat_history VALUES (?,?,?,?,?,?)', [
            (i, sid, f'Требование {i}: сохранить библиотеку', f'Обсудили шаг {i}', 1, '{"evidence_manifest": {"source": "test"}}') for i in range(1, 9)])
    return sid, path


@pytest.fixture
def conversation(catalog_engine, seeded_conversation):
    return seeded_conversation




def test_forget_blocks_every_old_recall_but_preserves_history_and_other_chats(conversation):
    sid, path = conversation
    service.update_context(sid, expected_revision=0, summary='Old', through_id=4)
    result = service.forget_context(sid, expected_revision=1)
    assert result['cutoff_id'] == 8 and not result['summary']
    assert memory_service.session_memory_items(sid) == []
    assert memory_service.session_user_questions(sid) == []
    assert memory_service.session_recent_retrieval_traces(sid) == []
    assert len(service.history_turns(sid)) == 8
    assert service.get_context('another')['cutoff_id'] == 0
    with sqlite3.connect(path) as conn:
        conn.execute('INSERT INTO chat_history VALUES (9,?,?,?,1,?)', (sid, 'Новая цель', 'Продолжим', '{}'))
    assert memory_service.session_user_questions(sid) == ['Новая цель']


def test_message_isolation_applies_to_every_projection_and_restores_original(seeded_conversation):
    sid, path = seeded_conversation
    with sqlite3.connect(path) as conn:
        conn.execute("ALTER TABLE chat_history ADD COLUMN attachment_context TEXT DEFAULT 'Файл'")
        conn.execute("ALTER TABLE chat_history ADD COLUMN feedback_status TEXT DEFAULT 'correct'")
        conn.execute("ALTER TABLE chat_history ADD COLUMN crag_status TEXT DEFAULT 'VERIFIED'")
    service.update_context(sid, expected_revision=0, summary='Копия старого ответа', through_id=7)

    def isolate(role, excluded=True):
        return service.catalog_call(str(path), 'context_isolate', session_id=sid, history_id=7,
                                    role=role, excluded=excluded)

    assert isolate('assistant') == [{'history_id': 7, 'role': 'assistant'}]
    assert service.summary_record(sid) is None
    with pytest.raises(service.ContextConflict):
        service.update_context(sid, expected_revision=1, summary='Устаревший результат модели')
    item = memory_service.session_memory_items(sid)[-2]
    assert item['turn_id'] == 'chat:7' and item['question'] and item['answer'] == ''
    assert item['attachment_context'] == 'Файл'
    assert 'Обсудили шаг 7' not in str(memory_service.session_dialogue_messages(sid))
    assert 'Обсудили шаг 7' not in str(service.history_turns(sid))
    assert len(memory_service.session_recent_retrieval_traces(sid, max_turns=8)) == 7
    assert 7 not in [row['id'] for row in service.catalog_call(str(path), 'history_recall', question='Другое')]

    isolate('user')
    assert not any('Требование 7' in question for question in memory_service.session_user_questions(sid, max_turns=8))
    assert not any(row['id'] == 7 for row in service.history_turns(sid))
    assert isolate('assistant', False) == [{'history_id': 7, 'role': 'user'}]
    item = memory_service.session_memory_items(sid)[-2]
    assert item['question'] == item['attachment_context'] == '' and item['answer'] == 'Обсудили шаг 7'
    assert isolate('user', False) == []
    with sqlite3.connect(path) as conn:
        assert conn.execute('SELECT question,answer FROM chat_history WHERE id=7').fetchone() == (
            'Требование 7: сохранить библиотеку', 'Обсудили шаг 7')










def test_summary_batch_does_not_claim_unread_turns():
    rows = [{'id': 1, 'question': 'a', 'answer': 'b'}, {'id': 2, 'question': 'x' * 1000, 'answer': 'y'}]
    text, through = service.summary_batch(rows, '', max_chars=100)
    assert through == 1 and 'xxxx' not in text
    with pytest.raises(ValueError, match='слишком длинное'):
        service.summary_batch(rows[1:], '', max_chars=100)


def test_context_api_validates_scope_and_concurrent_edits(conversation):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient
    from proxy.routers.workspace_memory import router
    from proxy.security import require_user
    sid, _ = conversation
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[require_user] = lambda: {'role': 'user'}
    with TestClient(app) as client:
        url = f'/api/workspace/memory/context/{sid}'
        assert client.get(url).json()['remembered_turns'] == 8
        assert client.patch(url, json={'expected_revision': 0, 'summary': 'Edited'}).json()['summary'] == 'Edited'
        assert client.patch(url, json={'expected_revision': 0, 'summary': 'Stale'}).status_code == 409
        assert client.post(url + '/forget', json={'expected_revision': 1}).json()['cutoff_id'] == 8
        assert client.get('/api/workspace/memory/context/missing').status_code == 404


@pytest.mark.parametrize('text,expected', [('Короткая заметка: встреча в библиотеке 17 мая.', 1), ('Шум', 0)])
def test_short_complete_note_is_indexable_without_lowering_large_document_floor(tmp_path, monkeypatch, text, expected):
    from backend import qdrant_adapter as module
    monkeypatch.setattr(module.support, 'convert_to_markdown_for_indexing', lambda *args, **kwargs: text)
    parser = SimpleNamespace(get_nodes_from_documents=lambda docs: [SimpleNamespace(text=text, node_id='n1', metadata={})])
    backend = object.__new__(module.QdrantLlamaIndexAdapter)
    result = backend._sync_markdown_nodes(tmp_path / 'note.txt', 'note.txt', 'test', parser, None)
    assert len(result) == expected
    if result: assert result[0]['text'] == text


def test_internal_preprocessing_files_are_not_documents():
    from backend.smart_index import is_temporary_source_name
    assert is_temporary_source_name('folder/.pdf_preprocess_state.json')
    assert is_temporary_source_name('folder/_les_dataset_profile.json')
    assert not is_temporary_source_name('folder/Заметка.txt')


@pytest.mark.asyncio
@pytest.mark.parametrize('outcome', ['saved', 'conflict'])
async def test_memory_editor_preserves_text_on_failure_and_uses_revision(monkeypatch, outcome, tmp_path):
    from nicegui import ui, core
    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    monkeypatch.setattr(ui, 'timer', lambda *_args, **_kwargs: SimpleNamespace(activate=lambda: None, deactivate=lambda: None))
    from nicegui.client import Client
    from nicegui.page import page
    from sovushka.components import conversation_memory as component
    state = dict(service.catalog_call(str(tmp_path/'missing.db'), 'context_get', session_id='fixture'), remembered_turns=8, summarized_turns=0)
    calls = []
    async def get(route): return dict(state)
    async def patch(route, body):
        calls.append(body)
        if outcome == 'conflict': return None
        state.update(summary=body['summary'], revision=1)
        return dict(state)
    monkeypatch.setattr(component, 'api_get', get)
    monkeypatch.setattr(component, 'api_patch', patch)
    with Client(page('/__memory')) as client:
        await component.build_conversation_memory('chat #1')
        field = next(e for e in client.elements.values() if e._props.get('label') == 'Что ЛЕС помнит о разговоре')
        field.set_value('Моя правка')
        button = next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == 'Сохранить память')
        listener = next(iter(button._event_listeners.values())).handler
        result = listener(SimpleNamespace(sender=button, client=client, args={}))
        if hasattr(result, '__await__'): await result
        for _ in range(8): await asyncio.sleep(0)
        assert calls[0]['expected_revision'] == 0 and calls[0]['summary'] == 'Моя правка'
        assert field.value == 'Моя правка' and button.enabled
        texts = [str(getattr(e, 'text', '')) for e in client.elements.values()]
        assert any('не подтверждены' in text for text in texts) == (outcome == 'conflict')
