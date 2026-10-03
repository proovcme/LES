"""Persistent, opt-in folder monitoring; failed scans never trigger synchronization."""
from __future__ import annotations

import asyncio
from contextlib import contextmanager
import json
import logging
import sqlite3
import time
from pathlib import Path

from backend.runtime_paths import mutable_path
from backend.light_processes import InstanceLock

logger = logging.getLogger(__name__)


class DatasetWatcher:
    def __init__(self, db_path=None):
        self.path = Path(db_path or mutable_path("data/dataset_watch.db"))
        self.lock = asyncio.Lock()

    @contextmanager
    def connect(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        conn = sqlite3.connect(self.path, timeout=5)
        try:
            conn.row_factory = sqlite3.Row
            conn.executescript("""
            CREATE TABLE IF NOT EXISTS watches (
                dataset_id TEXT PRIMARY KEY, path TEXT NOT NULL, enabled INTEGER NOT NULL,
                auto_index INTEGER NOT NULL, checked_at REAL, status TEXT NOT NULL DEFAULT 'waiting',
                pending TEXT NOT NULL DEFAULT '', stable_since REAL NOT NULL DEFAULT 0,
                message TEXT NOT NULL DEFAULT '');
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY, dataset_id TEXT NOT NULL, at REAL NOT NULL,
                kind TEXT NOT NULL, file_name TEXT NOT NULL, message TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS events_dataset_id ON events(dataset_id, id);
        """)
            with conn:
                yield conn
        finally:
            conn.close()

    def configure(self, dataset_id, path, *, enabled, auto_index):
        with self.connect() as conn:
            conn.execute("""INSERT INTO watches(dataset_id,path,enabled,auto_index) VALUES(?,?,?,?)
                ON CONFLICT(dataset_id) DO UPDATE SET path=excluded.path,enabled=excluded.enabled,
                auto_index=excluded.auto_index,pending='',stable_since=0,status='waiting'""",
                (dataset_id, str(path), int(enabled), int(auto_index)))
            self._event(conn, dataset_id, "settings", "", "Наблюдение включено" if enabled else "Наблюдение выключено")
        return self.status(dataset_id)

    def _event(self, conn, dataset_id, kind, file_name, message):
        self._events(conn, dataset_id, [(kind, file_name, message)])

    def _events(self, conn, dataset_id, events):
        """One filesystem scan writes and trims its journal as a single batch."""
        stamp = time.time()
        conn.executemany("INSERT INTO events(dataset_id,at,kind,file_name,message) VALUES(?,?,?,?,?)",
                         ((dataset_id, stamp, kind, file_name, message) for kind, file_name, message in events))
        conn.execute("DELETE FROM events WHERE dataset_id=? AND id NOT IN (SELECT id FROM events WHERE dataset_id=? ORDER BY id DESC LIMIT 1000)", (dataset_id, dataset_id))

    def status(self, dataset_id):
        with self.connect() as conn:
            row = conn.execute("SELECT * FROM watches WHERE dataset_id=?", (dataset_id,)).fetchone()
            events = conn.execute("SELECT * FROM events WHERE dataset_id=? ORDER BY id DESC LIMIT 100", (dataset_id,)).fetchall()
        watch = dict(row) if row else {"dataset_id": dataset_id, "enabled": False, "auto_index": False, "status": "disabled"}
        watch.pop("pending", None)
        return {"watch": watch, "events": [dict(event) for event in events]}

    async def tick(self, check, synchronize, *, now=None, settle_seconds=10):
        # Every process uses the same OS-owned lock. A crashed process releases it;
        # a database flag or an asyncio.Lock alone cannot provide that guarantee.
        ownership = InstanceLock(self.path.with_suffix(self.path.suffix + ".lock"))
        if not ownership.acquire():
            return
        try:
            await self._tick_owned(check, synchronize, now=now, settle_seconds=settle_seconds)
        finally:
            ownership.release()

    async def _tick_owned(self, check, synchronize, *, now=None, settle_seconds=10):
        if self.lock.locked():
            return
        async with self.lock:
            with self.connect() as conn:
                watches = [dict(row) for row in conn.execute("SELECT * FROM watches WHERE enabled=1")]
            for watch in watches:
                stamp = time.time() if now is None else now
                dataset_id = watch["dataset_id"]
                try:
                    diff = await check(watch)
                    changes = diff["_files"]
                    # Registry status/chunk counters change during processing, not
                    # because the source changed. They must not submit it again.
                    fingerprint = json.dumps({kind: [
                        {key: value for key, value in row.items() if key != "previous"}
                        for row in rows] for kind, rows in changes.items()},
                        sort_keys=True, ensure_ascii=False)
                    has_changes = any(changes.values())
                    stable = fingerprint == watch["pending"]
                    status = "changes" if has_changes else "current"
                    message = "Есть изменения" if has_changes else "Изменений нет"
                    already_submitted = stable and watch["status"] in {"indexing", "synced"}
                    if already_submitted and has_changes:
                        status, message = watch["status"], watch["message"]
                    if has_changes and not stable:
                        with self.connect() as conn:
                            self._events(conn, dataset_id, (
                                (kind, row['file_name'], {'new': 'Добавлен', 'changed': 'Изменён', 'deleted': 'Удалён'}[kind])
                                for kind, rows in changes.items() for row in rows
                            ))
                    if has_changes and stable and not already_submitted and watch["auto_index"] and stamp - watch["stable_since"] >= settle_seconds:
                        current = self.status(dataset_id)["watch"]
                        if not current["enabled"] or not current["auto_index"] or current["path"] != watch["path"]:
                            continue
                        result = await synchronize(watch)
                        status = "indexing" if result.get("parse_started") else "synced"
                        message = "Изменения переданы на индексацию" if result.get("parse_started") else "Изменения синхронизированы"
                        with self.connect() as conn:
                            self._event(conn, dataset_id, status, "", message)
                    with self.connect() as conn:
                        conn.execute("UPDATE watches SET checked_at=?,status=?,message=?,pending=?,stable_since=? WHERE dataset_id=?",
                                     (stamp, status, message, fingerprint, watch["stable_since"] if stable else stamp, dataset_id))
                except Exception as error:
                    logger.warning("Dataset watch paused for %s: %s", dataset_id, error)
                    message = str(getattr(error, "detail", "Проверка папки приостановлена. Проверьте доступ к папке и хранилищу."))
                    with self.connect() as conn:
                        if watch["status"] != "unavailable" or watch["message"] != message:
                            self._event(conn, dataset_id, "unavailable", "", message)
                        conn.execute("UPDATE watches SET checked_at=?,status='unavailable',message=?,pending='',stable_since=0 WHERE dataset_id=?", (stamp, message, dataset_id))


_watcher = None


def watcher():
    global _watcher
    if _watcher is None:
        _watcher = DatasetWatcher()
    return _watcher


async def watch_loop():
    from proxy.routers.datasets import ExternalDatasetSyncRequest, _external_dataset_diff, sync_external_dataset, parse_memory_state
    from fastapi import HTTPException
    from proxy.storage.file_storage import validate_external_source

    async def check(watch):
        memory = await parse_memory_state()
        if memory["state"] in {"RED", "CRITICAL", "UNKNOWN"}:
            raise HTTPException(503, "Недостаточно свободной памяти для обработки папки. Наблюдение возобновится после освобождения памяти.")
        root = validate_external_source(watch["path"])
        return await asyncio.to_thread(_external_dataset_diff, watch["dataset_id"], root)

    async def synchronize(watch):
        memory = await parse_memory_state()
        if memory["state"] in {"RED", "CRITICAL", "UNKNOWN"}:
            raise HTTPException(503, "Автоиндексация ожидает освобождения памяти.")
        return await sync_external_dataset(ExternalDatasetSyncRequest(path=watch["path"], dataset_id=watch["dataset_id"]), _admin=None)

    while True:
        try:
            await watcher().tick(check, synchronize)
        except Exception:
            logger.exception("Dataset watcher registry unavailable; retrying in 15 seconds")
        await asyncio.sleep(15)
