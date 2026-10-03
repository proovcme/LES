"""Actual SSE lifecycle: early tokens, reset, cancellation, partial persistence."""
import asyncio
import json
import pytest
from proxy.routers import chat
from proxy.services import chat_request_service, chat_persistence_service


@pytest.mark.asyncio
async def test_disconnect_cancels_own_provider_request(monkeypatch):
    stopped = asyncio.Event()
    async def generate(req, token_sink):
        try:
            await token_sink({'event':'token','data':'Первый фрагмент'})
            await asyncio.Event().wait()
        finally:
            stopped.set()
    monkeypatch.setattr(chat_request_service, '_run_chat_with_provider', generate)
    response = await chat.chat_stream(chat.ChatRequest(question='Тест'), _user=None)
    assert 'progress' in await anext(response.body_iterator)
    assert 'Первый фрагмент' in await anext(response.body_iterator)
    assert not stopped.is_set()
    await response.body_iterator.aclose()
    await asyncio.wait_for(stopped.wait(), 1)


@pytest.mark.asyncio
async def test_reset_does_not_resurrect_old_text_and_partial_is_not_success(monkeypatch):
    captured = {}
    def save(**values):
        captured.update(values)
        return 73
    async def generate(req, token_sink):
        await token_sink({'event':'token','data':'Старый ответ ' * 100})
        await token_sink({'event':'reset','data':{}})
        await token_sink({'event':'token','data':'Новый фрагмент'})
        raise RuntimeError('provider interrupted')
    monkeypatch.setattr(chat_request_service, '_run_chat_with_provider', generate)
    monkeypatch.setattr(chat_persistence_service, 'save_chat_history', save)
    response = await chat.chat_stream(chat.ChatRequest(question='Тест',session_id='synthetic'), _user=None)
    frames = [frame async for frame in response.body_iterator]
    final = json.loads(next(frame for frame in frames if frame.startswith('event: final')).split('data: ',1)[1])
    assert final['answer'] == 'Новый фрагмент'
    assert final['partial'] is True
    assert final['completion_status'] == 'interrupted'
    assert final['history_id'] == 73
    assert captured['success'] is False
    assert captured['validation_enabled'] is False


@pytest.mark.asyncio
async def test_non_streaming_provider_emits_complete_answer_once(monkeypatch):
    async def generate(req, token_sink):
        return {'answer':'Целый ответ','sources':[]}
    monkeypatch.setattr(chat_request_service, '_run_chat_with_provider', generate)
    response = await chat.chat_stream(chat.ChatRequest(question='Тест'), _user=None)
    frames = [frame async for frame in response.body_iterator]
    tokens = [frame for frame in frames if frame.startswith('event: token')]
    assert len(tokens) == 1
    assert json.loads(tokens[0].split('data: ',1)[1]) == 'Целый ответ'
