"""Human-readable operation stages; percentages only when work is measurable."""
import logging

logger = logging.getLogger(__name__)


async def chat_progress(sink, stage: str, label: str, *, completed=None, total=None):
    logger.info("[CHAT/STAGE] %s", stage)
    if sink is None:
        return
    payload = {"stage": stage, "label": label}
    if isinstance(total, int) and total > 0 and isinstance(completed, int):
        payload.update(completed=max(0, min(completed, total)), total=total)
    await sink({"event": "progress", "data": payload})
