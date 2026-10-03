"""Shared backend and job state for dataset operations."""
from __future__ import annotations
import asyncio
import logging
import sqlite3
from dataclasses import dataclass
from typing import Any




logger = logging.getLogger(__name__)


def _table_exists(conn: sqlite3.Connection, table: str) -> bool:
    row = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
        (table,),
    ).fetchone()
    return bool(row)


@dataclass
class DatasetRouterState:
    rag_backend: Any
    job_service: Any
    job_tracker: dict
    log_history: Any
    parse_semaphore: asyncio.Semaphore
    sync_parse_semaphore: asyncio.Semaphore
    current_mode: dict[str, Any] | None = None

    @property
    def backend(self):
        res = self.rag_backend() if callable(self.rag_backend) else self.rag_backend
        if res is None:
            try:
                import proxy.app as _papp
                return _papp.rag_backend
            except Exception:
                pass
        return res


_state: DatasetRouterState | None = None


def set_dataset_state(state: DatasetRouterState) -> None:
    global _state
    _state = state


def get_dataset_state() -> DatasetRouterState:
    global _state
    if _state is None:
        try:
            from proxy.app import configure_router_state
            configure_router_state()
        except Exception:
            pass
    if _state is None:
        from proxy.services.job_service import job_service
        _state = DatasetRouterState(
            rag_backend=lambda: None,
            job_service=job_service,
            job_tracker={},
            log_history=None,
            parse_semaphore=asyncio.Semaphore(1),
            sync_parse_semaphore=asyncio.Semaphore(1),
        )
    return _state
