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
def conversation(tmp_path, monkeypatch):
    path = tmp_path / 'meta.db'
    for module in (service, chat_session_service, memory_service):
        monkeypatch.setattr(module, 'rag_meta_db_path', lambda: str(path))
    service._locks.clear()
    sid = chat_session_service.create_session(title='Контекст')['session_id']
    with sqlite3.connect(path) as conn:
        conn.execute('CREATE TABLE chat_history (id INTEGER PRIMARY KEY, session_id TEXT, question TEXT, answer TEXT, success INTEGER, retrieval_trace_json TEXT)')
        conn.executemany('INSERT INTO chat_history VALUES (?,?,?,?,?,?)', [
            (i, sid, f'Требование {i}: сохранить библиотеку', f'Обсудили шаг {i}', 1, '{"evidence_manifest": {"source": "test"}}') for i in range(1, 9)])
    return sid, path


@pytest.mark.asyncio
async def test_summary_compacts_old_turns_keeps_recent_and_survives_restart(conversation, monkeypatch):
    sid, path = conversation
    calls = []
    async def generate(previous, turns):
        calls.append(turns)
        return 'Сохранить библиотеку. Обсуждены шаги 1–6.', turns[-1]['id'], 'model:r1'
    monkeypatch.setattr(service, '_generate', generate)
    result = await service.summarize(sid)
    assert result['through_id'] == 6
    assert [row['id'] for row in calls[0]] == list(range(1, 7))
    assert len(service.history_turns(sid)) == 8
    assert [row['turn_id'] for row in memory_service.session_memory_items(sid)] == ['chat:7', 'chat:8']
    assert 'не источник доказательств' in memory_service.session_memory(sid)
    assert service.summary_record(sid)['is_evidence'] is False
    service._locks.clear()
    assert service.context_status(sid)['summarized_turns'] == 6
    assert await service.summarize(sid) == result
    assert len(calls) == 1


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


@pytest.mark.asyncio
async def test_concurrent_forget_cannot_be_overwritten_by_inflight_summary(conversation, monkeypatch):
    sid, _ = conversation
    started, done = asyncio.Event(), asyncio.Event()
    async def generate(previous, turns):
        started.set()
        await done.wait()
        return 'Must not return', 6, 'model:r1'
    monkeypatch.setattr(service, '_generate', generate)
    task = asyncio.create_task(service.summarize(sid))
    await started.wait()
    service.forget_context(sid, expected_revision=0)
    done.set()
    result = await task
    assert result['summary'] == '' and result['cutoff_id'] == 8


@pytest.mark.asyncio
async def test_summary_failure_preserves_previous_and_does_not_leak_errors(conversation, monkeypatch):
    sid, _ = conversation
    service.update_context(sid, expected_revision=0, summary='Previously saved', through_id=2)
    async def broken(*args): raise RuntimeError('secret transport details')
    monkeypatch.setattr(service, '_generate', broken)
    result = await service.summarize(sid, force=True)
    assert result['summary'] == 'Previously saved' and result['through_id'] == 2
    assert 'История сохранена' in result['last_error'] and 'secret' not in result['last_error']


@pytest.mark.asyncio
async def test_summary_waits_for_shared_model_slot_and_releases_it(conversation, monkeypatch):
    sid, _ = conversation
    semaphore = asyncio.Semaphore(1)
    await semaphore.acquire()
    calls = []
    connection = SimpleNamespace(locality='loopback', revision_id='selected:r1',
                                 effective_preset=SimpleNamespace(input_token_limit=8192))
    runtime = SimpleNamespace(current_mode={'mode': 'chat'}, metrics_cache={'ram_free_gb': 12},
                              job_service=None, job_tracker={}, llm_semaphore=semaphore)
    monkeypatch.setattr('proxy.services.chat_runtime.get_chat_state', lambda: runtime)
    monkeypatch.setattr('proxy.services.chat_runtime._active_dispatcher_reindex_jobs', lambda _: 0)
    monkeypatch.setattr('proxy.services.generation_guard_service.live_memory_metrics', lambda metrics:dict(metrics))
    monkeypatch.setattr('proxy.services.model_connection_resolver_service.ModelConnectionResolver.resolve',
                        lambda *args, **kwargs: connection)
    async def complete(self, actual, request):
        calls.append(actual.revision_id)
        assert semaphore.locked()
        return SimpleNamespace(text='{"keep":[0]}', finish_reason='stop')
    monkeypatch.setattr('proxy.services.openai_compatible_transport_service.OpenAICompatibleTransport.complete', complete)
    task = asyncio.create_task(service.summarize(sid))
    for _ in range(5): await asyncio.sleep(0)
    assert calls == []
    semaphore.release()
    assert (await task)['summary'] == 'Пользователь: Требование 1: сохранить библиотеку'
    assert calls == ['selected:r1']
    assert not semaphore.locked()


@pytest.mark.asyncio
async def test_disabled_memory_does_not_generate_or_recall(conversation, monkeypatch):
    sid, _ = conversation
    async def forbidden(*args): pytest.fail('Disabled memory must not call a model')
    monkeypatch.setattr(service, '_generate', forbidden)
    service.update_context(sid, expected_revision=0, enabled=False, summary='Hidden')
    await service.summarize(sid, force=True)
    assert service.summary_record(sid) is None
    assert memory_service.session_dialogue_messages(sid) == []


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
async def test_memory_editor_preserves_text_on_failure_and_uses_revision(monkeypatch, outcome):
    from nicegui import ui, core
    monkeypatch.setattr(core, 'loop', asyncio.get_running_loop())
    monkeypatch.setattr(ui, 'timer', lambda *_args, **_kwargs: SimpleNamespace(activate=lambda: None, deactivate=lambda: None))
    from nicegui.client import Client
    from nicegui.page import page
    from sovushka.components import conversation_memory as component
    state = dict(service._DEFAULT, remembered_turns=8, summarized_turns=0)
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
