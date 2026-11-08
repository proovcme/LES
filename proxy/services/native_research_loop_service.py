"""Async IO bindings for Rust research control, with request-owned cancellation.

Prompt assembly and special tool handlers remain separate retained adapters.
Final generation retries and research admission are both Rust-owned.
"""
import asyncio
from concurrent.futures import TimeoutError as FutureTimeout
import threading

from backend import native_core
from proxy.services.chat_section_context_service import SectionReadError


async def _run_bound(request, model, tool=None, *, timeout_code):
    loop = asyncio.get_running_loop()
    cancelled = threading.Event()
    active = [None]
    lock = threading.Lock()
    failures = []

    def exchange(operation, value):
        if cancelled.is_set():
            return {'error': {'code': 'CHAT_CANCELLED'}}
        if operation == 0:
            action = model(value)
        elif operation == 1 and tool is not None:
            action = tool(value['call'])
        else:
            return {'error': {'code': 'CHAT_IO_FAILED'}}
        future = asyncio.run_coroutine_threadsafe(action, loop)
        with lock:
            active[0] = future
            if cancelled.is_set():
                future.cancel()
        try:
            return future.result(timeout=max(.001, value['timeout_ms'] / 1000))
        except FutureTimeout:
            # asyncio.TimeoutError and concurrent.futures.TimeoutError are the
            # same builtin type. A handler's own timeout must keep its cause;
            # only an unfinished future means the native turn budget expired.
            if future.done() and not future.cancelled():
                original = future.exception()
                if original is not None:
                    failures.append(original)
                    detail = getattr(original, 'detail', None)
                    code = detail.get('code') if isinstance(detail, dict) else None
                    return {'error': {'code': code or 'CHAT_IO_FAILED'}}
            future.cancel()
            error = SectionReadError(503, detail={'code': timeout_code})
            failures.append(error)
            return {'error': {'code': timeout_code}}
        except BaseException as error:
            failures.append(error)
            detail = getattr(error, 'detail', None)
            code = detail.get('code') if isinstance(detail, dict) else None
            return {'error': {'code': code or 'CHAT_IO_FAILED'}}
        finally:
            with lock:
                if active[0] is future:
                    active[0] = None

    task = asyncio.create_task(asyncio.to_thread(native_core.run_research, request, exchange))
    try:
        result = await asyncio.shield(task)
    except asyncio.CancelledError:
        cancelled.set()
        with lock:
            if active[0] is not None:
                active[0].cancel()
        try:
            await asyncio.wait_for(asyncio.shield(task), 5)
        except (Exception, asyncio.CancelledError):
            pass
        raise
    if failures:
        raise failures[0]
    if result['status'] == 'failed':
        raise SectionReadError(503, detail={'code': result.get('code') or 'SOURCE_READ_FAILED'})
    return result


async def run_research(*, allowed_tools, max_calls, deadline_seconds, binding, model, tool):
    return await _run_bound({
        'schema': 'les.chat-research.v1', 'allowed_tools': sorted(allowed_tools),
        'max_calls': max_calls, 'deadline_seconds': deadline_seconds, 'binding': binding,
    }, model, tool, timeout_code='RESEARCH_DEADLINE_EXCEEDED')


async def run_answer(*, model, max_attempts=2, deadline_seconds=900.):
    return await _run_bound({
        'schema': 'les.chat-answer.v1', 'max_attempts': max_attempts,
        'deadline_seconds': deadline_seconds,
    }, model, timeout_code='ANSWER_DEADLINE_EXCEEDED')
