"""Legacy request activity codec. The durable idle worker belongs to Rust."""
from contextlib import asynccontextmanager
import logging
from proxy.services import local_model_http

logger = logging.getLogger(__name__)


@asynccontextmanager
async def foreground_request(session_id):
    # A standalone compatibility caller has no native idle worker to preempt.
    # It never starts a replacement Python scheduler or generates a summary.
    if not local_model_http.web_ready():
        yield
        return
    lease = await local_model_http.context_request('foreground_begin')
    try:
        yield
    finally:
        try:
            await local_model_http.context_request('foreground_end', lease=lease['lease'], session_id=session_id)
        except Exception:
            logger.error('Native summary activity release failed: SUMMARY_STORAGE_UNAVAILABLE')
