import asyncio
import sqlite3
from types import SimpleNamespace

import pytest
from proxy.services import chat_durability_service as durable
from proxy.services import chat_persistence_service as persistence
from proxy.services import chat_session_service as sessions
from proxy.services import conversation_context_service as context
from proxy.services import memory_service as memory


@pytest.fixture
def history(tmp_path, monkeypatch):
    path = tmp_path / 'meta.db'
    for module in (durable, persistence, sessions, context, memory):
        monkeypatch.setattr(module, 'rag_meta_db_path', lambda: str(path))
    monkeypatch.setattr(persistence, 'update_chat_profile', lambda **kwargs: None)
    sid = sessions.create_session(title='ВОР, 70 позиций')['session_id']
    question = '\n'.join(f'{i:03d} | Работа №{i} 日本語 | м² | {i}.25' for i in range(1, 71))
    request = SimpleNamespace(session_id=sid, question=question, attachment_context='Единицы: м²')
    return path, request


def row(path):
    with sqlite3.connect(path) as conn:
        return conn.execute('SELECT question,answer,crag_status,success FROM chat_history').fetchall()


def test_restart_preserves_every_visible_fragment_and_never_promotes_partial(history):
    path, request = history
    key = durable.begin(request)
    durable.checkpoint(key, 'token', 'Ошибка черновика')
    durable.checkpoint(key, 'reset', {})
    for fragment in ['Позиции 1–5: ', 'сохранены. ', '日本語 😀']:
        durable.checkpoint(key, 'token', fragment)
    assert durable.interrupt() == 1
    assert durable.interrupt() == 0
    persistence.ensure_chat_history_schema(path)
    assert row(path) == [(request.question, 'Позиции 1–5: сохранены. 日本語 😀', 'INTERRUPTED', 0)]
    assert memory.session_memory_items(request.session_id) == []
    assert context.history_turns(request.session_id) == []


def test_final_answer_updates_exactly_one_row(history):
    path, request = history
    key = durable.begin(request)
    durable.checkpoint(key, 'token', 'Начало')
    binding = durable._history_id.set(key)
    try:
        saved = persistence.save_chat_history(question=request.question, answer='Полный ответ',
            sources=[], crag_status='UNVALIDATED', latency_sec=1, tokens=3, session_id=request.session_id)
    finally:
        durable._history_id.reset(binding)
    assert saved == key
    assert durable.interrupt() == 0
    assert row(path) == [(request.question, 'Полный ответ', 'UNVALIDATED', 1)]


def test_failure_survives_reopen_and_cannot_accept_more_tokens(history):
    from backend.workspace_native import catalog_call
    path, request=history
    key=durable.begin(request)
    durable.checkpoint(key,'token','Неполный текст 日本語')
    failure={'code':'PDF_REVISION_CHANGED','detail':'PDF изменился. Проверка остановлена.'}
    durable.checkpoint(key,'error',failure)
    durable.checkpoint(key,'token','Это не должно сохраниться')
    assert durable.interrupt()==0
    messages=catalog_call(path,'chat_history_messages',session_id=request.session_id)
    answer=messages[-1]
    assert answer['text']=='Неполный текст 日本語'
    assert answer['crag']=='FAILED' and answer['meta']['error']==failure
    assert answer['meta']['partial'] is True
    assert context.history_turns(request.session_id)==[]
    binding=durable._history_id.set(key)
    try:
        with pytest.raises(LookupError):
            persistence.save_chat_history(question=request.question,answer='Late success',sources=[],
                crag_status='UNVALIDATED',latency_sec=1,tokens=1,session_id=request.session_id)
    finally:
        durable._history_id.reset(binding)
    assert row(path)[0][2:] == ('FAILED',0)


@pytest.mark.asyncio
async def test_public_error_is_committed_before_delivery_and_restored_after_restart(history,monkeypatch):
    from fastapi import HTTPException
    from proxy.routers import chat,chat_history
    from proxy.services import chat_request_service
    path, request=history
    monkeypatch.setattr(chat_history,'rag_meta_db_path',lambda:str(path))
    async def generate(req,token_sink):
        raise HTTPException(422,detail={'code':'PDF_PAGE_INCOMPLETE'})
    monkeypatch.setattr(chat_request_service,'_run_chat_with_provider',generate)
    response=await chat.chat_stream(chat.ChatRequest(**vars(request)),_user=None)
    events=[]
    async for event in response.body_iterator:
        events.append(event)
        if 'event: error' in event:
            assert row(path)[0][2:] == ('FAILED',0)
    assert not any('event: token' in event or 'event: final' in event for event in events)
    durable.interrupt()
    restored=(await chat_history.get_chat_history(session_id=request.session_id,_user=None))[-1]
    assert restored['text']=='' and restored['meta']['partial'] is False
    assert restored['meta']['error']['code']=='PDF_PAGE_INCOMPLETE'
    assert 'прочитана не полностью' in restored['meta']['error']['detail']


@pytest.mark.asyncio
async def test_sse_disconnect_leaves_durable_text(history, monkeypatch):
    from proxy.routers import chat
    from proxy.services import chat_request_service
    path, request = history
    async def generate(req, token_sink):
        await token_sink({'event': 'token', 'data': 'Полученный текст'})
        await asyncio.Event().wait()
    monkeypatch.setattr(chat_request_service, '_run_chat_with_provider', generate)
    response = await chat.chat_stream(chat.ChatRequest(**vars(request)), _user=None)
    await anext(response.body_iterator)
    assert 'Полученный текст' in await anext(response.body_iterator)
    assert row(path)[0][1] == 'Полученный текст'  # Committed before delivery.
    await response.body_iterator.aclose()
    assert row(path)[0][2:] == ('INTERRUPTED', 0)
