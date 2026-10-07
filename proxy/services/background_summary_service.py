"""Idle-only conversation compaction; a new chat request always takes priority."""
import asyncio
import logging
from contextlib import asynccontextmanager

logger = logging.getLogger(__name__)
_tasks: dict[str, asyncio.Task] = {}
_pending: set[str] = set()
_foreground = 0


async def stop():
    tasks = list(_tasks.values())
    for task in tasks:
        task.cancel()
    await asyncio.gather(*tasks, return_exceptions=True)
    _tasks.clear()


def schedule(session_id: str | None):
    if session_id:
        _pending.add(session_id)
    if _foreground:
        return
    for sid in tuple(_pending):
        if sid in _tasks:
            continue
        async def run(key=sid):
            try:
                await asyncio.sleep(2)
                from proxy.services.conversation_context_service import summarize
                await summarize(key)
                _pending.discard(key)
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception('Background conversation summary failed')
                _pending.discard(key)
            finally:
                _tasks.pop(key, None)
        _tasks[sid] = asyncio.create_task(run())


@asynccontextmanager
async def foreground_request(session_id):
    global _foreground
    _foreground += 1
    try:
        # Cancels only our idle summaries, never a manual action or another chat.
        await stop()
        yield
    finally:
        _foreground -= 1
        schedule(session_id)
