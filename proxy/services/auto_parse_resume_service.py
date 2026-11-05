"""Startup adapter: preflight before job creation, terminal outcome on every exit."""
import asyncio
import logging
from backend.rag_config import rag_meta_db_path
from proxy.services.dataset_contracts import ParseSchedulerRequest
from proxy.services.dataset_parse_service import (
    active_parse_scheduler_job, assert_parse_admission, claim_parse_scheduler,
    finish_scheduler_failure, pending_document_count, run_parse_scheduler,
)

logger = logging.getLogger(__name__)
_tasks = set()


def _pending_count():
    return pending_document_count(rag_meta_db_path())


async def resume_pending_parse(state):
    pending = await asyncio.to_thread(_pending_count)
    if not pending or active_parse_scheduler_job(state):
        return None
    req = ParseSchedulerRequest(batch_limit=1, max_batches=min(pending,10000), cooldown_sec=2.0,
        background=True, min_free_gb=2.5, unload_before_start=False,
        unload_between_batches=False, warm_embedder=True, unload_after_finish=False)
    # Missing/stale search connection or memory pressure must not create an active job.
    await assert_parse_admission(state, min_free_gb=req.min_free_gb)
    message = f'Auto-resumed indexing for {pending} pending files'
    job = claim_parse_scheduler(state, source='auto_resume', message=message, total=pending)
    if job is None:
        return None
    job_id = job['id']

    async def run():
        try:
            await run_parse_scheduler(state, req, job_id=job_id)
            remaining = await asyncio.to_thread(_pending_count)
            if remaining:
                logger.info('[AUTO_RESUME_PARSE] remaining=%s; supervisor will continue', remaining)
        except asyncio.CancelledError as error:
            finish_scheduler_failure(state, job_id, error, cancelled=True, automatic=True)
            raise
        except Exception as error:
            finish_scheduler_failure(state, job_id, error, automatic=True)
            logger.error('[AUTO_RESUME_PARSE %s] code=AUTO_RESUME_PARSE_FAILED type=%s message=%s',
                         job_id,type(error).__name__,str(error) or type(error).__name__,exc_info=True)

    task = asyncio.create_task(run())
    _tasks.add(task)
    task.add_done_callback(_tasks.discard)
    return task
