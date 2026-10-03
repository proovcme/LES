"""Shared chat state access; no HTTP or generation work."""
from __future__ import annotations
import logging
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)

@dataclass
class ChatRouterState:
    rag_backend: Any
    llm_semaphore: Any
    crag_stats: dict
    chat_metrics: dict
    reranker_available: bool
    reranker_cls: Any
    current_mode: dict[str, Any] | None = None
    metrics_cache: dict[str, Any] | None = None
    job_service: Any = None
    job_tracker: dict[str, Any] | None = None

    @property
    def backend(self):
        return self.rag_backend() if callable(self.rag_backend) else self.rag_backend


_state: ChatRouterState | None = None


def set_chat_state(state: ChatRouterState) -> None:
    global _state
    _state = state


def get_chat_state() -> ChatRouterState:
    if _state is None:
        raise RuntimeError("chat router state is not configured")
    return _state


def _active_dispatcher_reindex_jobs(state: chat_runtime.ChatRouterState) -> int:
    try:
        status = RuntimeDispatcher(
            current_mode=state.current_mode or {},
            metrics_cache=state.metrics_cache or {},
        ).reindex_status_payload()
    except Exception:
        return 0
    return 1 if status.get("running") else 0

from proxy.services.runtime_dispatcher import RuntimeDispatcher
