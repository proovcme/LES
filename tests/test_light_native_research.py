"""Rust owns research sequencing; async bindings preserve cancellation and original faults."""
import asyncio
import contextvars
import os
from types import MappingProxyType

import pytest

from proxy.services.native_research_loop_service import run_research
from proxy.services.native_research_loop_service import run_answer
from proxy.services.chat_section_context_service import SectionReadError


pytestmark = pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'), reason='Native research ABI required')


def selection(tool, args):
    import json
    return {'response': {'text': '', 'tool_calls': [{'id': 'call-1', 'function': {'name': tool, 'arguments': json.dumps(args)}}], 'selection_text': json.dumps(
        {'calls': [{'tool': tool, 'args': args}]})}, 'sources': [], 'epochs': {}}


def start(model, tool, *, allowed=('read_pdf_source',), maximum=6, deadline=10):
    return run_research(allowed_tools=allowed, max_calls=maximum, deadline_seconds=deadline,
        binding={'question': 'Проверь источник', 'dataset_ids': ['ds'], 'target': {}, 'sources': []},
        model=model, tool=tool)


def test_long_page_continues_without_extra_model_turn_and_preserves_context():
    async def scenario():
        request_context = contextvars.ContextVar('request_context', default='missing')
        request_context.set('private-request')
        owner = asyncio.get_running_loop()
        events = []

        async def model(turn):
            assert asyncio.get_running_loop() is owner
            assert request_context.get() == 'private-request'
            events.append('model')
            if events.count('model') == 1:
                return selection('read_pdf_source', {'doc_id': 'doc', 'page': 2})
            return {'response': {'text': 'Ответ со ссылкой на страницу 2', 'selection_text': 'Ответ',
                                 'tool_calls': [], 'usage': {'total_tokens': 25}}, 'sources': [], 'epochs': {}}

        async def tool(call):
            assert asyncio.get_running_loop() is owner
            offset = call['args'].get('offset', 0)
            events.append(offset)
            return {'payload': {'tool': 'read_pdf_source', 'status': 'ok', 'result': {
                'doc_id': 'doc', 'doc_name': 'Doc.pdf', 'dataset_id': 'ds', 'source_sha256': 'a' * 64,
                'page': 2, 'offset': offset, 'text': 'x' * 6, 'total_chars': 12,
                'next_offset': 6 if offset == 0 else None, 'complete': offset == 6}}, 'epochs': {}}

        result = await start(model, tool)
        assert result['status'] == 'completed'
        assert result['terminal']['usage']['total_tokens'] == 25
        assert events == ['model', 0, 6, 'model']
        assert result['rounds'][1]['model_sent'] is False
    asyncio.run(scenario())


def test_changed_source_stops_before_an_answer():
    async def scenario():
        calls = []
        async def model(turn):
            calls.append('model')
            return selection('read_pdf_source', {'doc_id': 'doc', 'page': 1})
        async def tool(call):
            calls.append('tool')
            return {'payload': {'tool': 'read_pdf_source', 'status': 'error',
                'result': {'code': 'PDF_REVISION_CHANGED'}}, 'epochs': {}}
        with pytest.raises(SectionReadError) as fault:
            await start(model, tool)
        assert fault.value.detail['code'] == 'PDF_REVISION_CHANGED'
        assert calls == ['model', 'tool']
    asyncio.run(scenario())


def test_original_error_survives_native_callback_boundary():
    async def scenario():
        original = SectionReadError(409, detail={'code': 'SOURCE_GENERATION_CHANGED', 'page': 9})
        async def model(turn):
            raise original
        async def tool(call):
            pytest.fail('No tool may run after model IO failure')
        with pytest.raises(SectionReadError) as fault:
            await start(model, tool)
        assert fault.value is original
        original = TimeoutError('A handler timeout, not the research deadline')
        with pytest.raises(TimeoutError) as fault:
            await start(model, tool)
        assert fault.value is original
    asyncio.run(scenario())


def test_deadline_cancels_pending_model_without_running_a_tool():
    async def scenario():
        cancelled = asyncio.Event()
        async def model(turn):
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()
        async def tool(call):
            pytest.fail('Expired turn must not execute')
        with pytest.raises(SectionReadError) as fault:
            await start(model, tool, deadline=.04)
        assert fault.value.detail['code'] == 'RESEARCH_DEADLINE_EXCEEDED'
        await asyncio.wait_for(cancelled.wait(), 1)
    asyncio.run(scenario())


def test_request_cancellation_stops_native_owner_and_tool_task():
    async def scenario():
        started, cancelled = asyncio.Event(), asyncio.Event()
        calls = []
        async def model(turn):
            calls.append('model')
            return selection('read_pdf_source', {'doc_id': 'doc', 'page': 1})
        async def tool(call):
            calls.append('tool')
            started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()
        task = asyncio.create_task(start(model, tool))
        await asyncio.wait_for(started.wait(), 1)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(task, 2)
        await asyncio.wait_for(cancelled.wait(), 1)
        assert calls == ['model', 'tool']
    asyncio.run(scenario())


def test_unknown_native_tool_does_not_turn_into_a_final_answer():
    async def scenario():
        async def model(turn):
            return selection('unregistered_tool', {})
        async def tool(call):
            pytest.fail('Unregistered tool must never execute')
        with pytest.raises(SectionReadError) as fault:
            await start(model, tool)
        assert fault.value.detail['code'] == 'TOOL_NOT_ALLOWED'
    asyncio.run(scenario())


def test_mixed_allowed_and_unknown_native_calls_fail_before_any_effect():
    async def scenario():
        async def model(turn):
            reply = selection('read_pdf_source', {'doc_id': 'doc', 'page': 1})
            reply['response']['tool_calls'].extend(selection('unregistered_tool', {})['response']['tool_calls'])
            return reply
        async def tool(call):
            pytest.fail('No partial batch may execute after an unknown native call')
        with pytest.raises(SectionReadError) as fault:
            await start(model, tool)
        assert fault.value.detail['code'] == 'TOOL_NOT_ALLOWED'
    asyncio.run(scenario())


def test_frozen_transport_calls_keep_ids_and_nested_arguments_across_the_abi():
    async def scenario():
        async def model(turn):
            reply = selection('agent_workspace', {'operation': 'read', 'area': 'user'})
            call = reply['response']['tool_calls'][0]
            call['function'] = MappingProxyType(call['function'])
            reply['response']['tool_calls'] = (MappingProxyType(call),)
            return reply
        seen = []
        async def tool(call):
            seen.append(call)
            return {'payload': {'tool': 'agent_workspace', 'status': 'ok', 'result': {}}, 'epochs': {}}
        result = await start(model, tool, allowed=('agent_workspace',))
        assert result['status'] == 'needs_final'
        assert seen == [{'tool': 'agent_workspace', 'call_id': 'call-1',
                         'args': {'operation': 'read', 'area': 'user'}}]
    asyncio.run(scenario())


def test_workspace_write_conflict_allows_reading_the_new_revision():
    async def scenario():
        turns = 0
        operations = []
        async def model(turn):
            nonlocal turns
            turns += 1
            if turns == 4:
                return {'response': {'text': 'Проверена новая редакция', 'tool_calls': []},
                        'sources': [], 'epochs': {}}
            return selection('agent_workspace', {'operation': 'edit' if turns == 2 else 'read',
                                                 'area': 'user'})
        async def tool(call):
            operations.append(call['args']['operation'])
            conflict = call['args']['operation'] == 'edit'
            return {'payload': {'tool': 'agent_workspace', 'status': 'error' if conflict else 'ok',
                'result': {'code': 'AGENT_CONFLICT' if conflict else '', 'changed': False},
                'execution': {'persisted': False}}, 'epochs': {}}
        result = await start(model, tool, allowed=('agent_workspace',))
        assert result['status'] == 'completed'
        assert operations == ['read', 'edit', 'read']
    asyncio.run(scenario())


def test_dynamic_document_revision_allows_reread_then_stops_replay():
    async def scenario():
        revision = 0
        calls = []
        async def model(turn):
            value = selection('document_task', {'action': 'read'})
            value['epochs'] = {'document_task': ['task', revision]}
            return value
        async def tool(call):
            nonlocal revision
            calls.append(revision)
            revision = 1
            return {'payload': {'tool': 'document_task', 'status': 'ok', 'result': {}},
                    'epochs': {'document_task': ['task', revision]}}
        result = await start(model, tool, allowed=('document_task',))
        assert calls == [0, 1]
        assert result['status'] == 'needs_final'
        assert result['stop_reason'] == 'repeated_read'
    asyncio.run(scenario())


def test_final_generation_retries_only_empty_text_and_preserves_a_valid_answer():
    async def scenario():
        calls = []
        async def model(turn):
            calls.append((turn['attempt'], turn['strict']))
            return {'response': {'text': '' if turn['attempt'] == 1 else 'Проверенный ответ',
                                 'tool_calls': [], 'usage': {'total_tokens': 20}}}
        result = await run_answer(model=model)
        assert calls == [(1, False), (2, True)]
        assert result['terminal']['text'] == 'Проверенный ответ'
        assert result['attempts'] == 2
    asyncio.run(scenario())


@pytest.mark.parametrize('reply,code', [
    ({'response': {'text': ' ', 'tool_calls': []}}, 'MODEL_EMPTY_RESPONSE'),
    ({'response': {'text': 'Сделал', 'tool_calls': [{}]}}, 'TOOL_BUDGET_EXHAUSTED'),
])
def test_failed_final_generation_cannot_be_published(reply, code):
    async def scenario():
        calls = []
        async def model(turn):
            calls.append(turn['attempt'])
            return reply
        with pytest.raises(SectionReadError) as fault:
            await run_answer(model=model)
        assert fault.value.detail['code'] == code
        assert calls == ([1, 2] if code == 'MODEL_EMPTY_RESPONSE' else [1])
    asyncio.run(scenario())
