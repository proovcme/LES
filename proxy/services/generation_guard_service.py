"""Admission and permits are checked for the actual primary or fallback invocation."""
from contextlib import asynccontextmanager

from fastapi import HTTPException
from proxy.services.runtime_admission import (
    acquire_generation_slot, count_active_jobs, evaluate_chat_admission, generation_semaphore, live_memory_metrics,
)


def ensure_generation_allowed(state, connection):
    from proxy.services.chat_runtime import _active_dispatcher_reindex_jobs
    admission = evaluate_chat_admission(
        current_mode=state.current_mode, metrics_cache=live_memory_metrics(state.metrics_cache),
        active_jobs=count_active_jobs(state.job_service, state.job_tracker) + _active_dispatcher_reindex_jobs(state),
        connection=connection,
    )
    if admission.allowed:
        return
    if any(item.startswith(('ram_free_gb=', 'swap_pct=')) for item in admission.failures):
        detail = {'code': 'CHAT_MEMORY_PRESSURE', 'detail':
                  'Сейчас недостаточно свободной памяти для ответа. Закройте ненужные приложения '
                  'или дождитесь завершения обработки документов и повторите запрос.'}
    elif admission.active_jobs:
        detail = {'code': 'CHAT_INDEXING_BUSY', 'detail':
                  'Сейчас обрабатываются документы. Дождитесь завершения обработки '
                  'или остановите очередь в разделе «Данные» и повторите запрос.'}
    else:
        detail = {'code': 'CHAT_GENERATION_PAUSED', 'detail':
                  'Ответы временно приостановлены настройками ресурсов. '
                  'Проверьте режим работы в настройках и повторите запрос.'}
    raise HTTPException(status_code=admission.status_code, detail=detail)


@asynccontextmanager
async def generation_guard(state, connection, *, token_sink=None):
    from proxy.services.operation_progress_service import chat_progress
    ensure_generation_allowed(state, connection)
    semaphore = generation_semaphore(state.llm_semaphore, connection=connection)
    await chat_progress(token_sink, 'queue', 'Ожидаю свободный слот модели')
    async with acquire_generation_slot(semaphore, timeout_seconds=45):
        # Memory/mode/jobs can change while this request is queued.
        ensure_generation_allowed(state, connection)
        await chat_progress(token_sink, 'generation', 'Модель готовит ответ')
        yield
