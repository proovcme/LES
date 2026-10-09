"""Durable recovery for the Qdrant/FTS/catalog file replacement boundary."""
from contextlib import contextmanager, closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import uuid

from qdrant_client import models
from backend.interface import EmbeddingContractError


def retire_previous(client, collection: str, dataset: str, file_name: str, keep_ids: list[str]):
    client.delete(collection_name=collection, wait=True, points_selector=models.FilterSelector(
        filter=models.Filter(must=[
            models.FieldCondition(key="dataset_id", match=models.MatchValue(value=dataset)),
            models.FieldCondition(key="file_name", match=models.MatchValue(value=file_name)),
        ], must_not=[models.HasIdCondition(has_id=keep_ids)])))


def discard_staged(client, collection: str, ids: list[str]):
    if ids:
        client.delete(collection_name=collection, wait=True, points_selector=models.PointIdsList(points=ids))


class ReplacementJournal:
    """A commit decision is durable *before* the old generation is retired.

    Before that decision recovery removes staged IDs; afterwards it completes
    retirement and rebuilds FTS from the accepted Qdrant records. The OS lease
    prevents two writers/recoverers operating on the same collection.
    """

    def __init__(self, content_dir, collection):
        self.collection = collection
        self.directory = Path(content_dir) / ".index-replacements" / hashlib.sha256(
            collection.encode("utf-8")).hexdigest()[:24]
        self.path = self.directory / "journal.sqlite"

    @classmethod
    def for_adapter(cls, adapter):
        return cls(adapter.content_dir, adapter.collection_name)

    def _read(self):
        if not self.path.exists():
            return None
        try:
            with sqlite3.connect(self.path) as conn:
                row = conn.execute("SELECT body FROM replacement WHERE id=1").fetchone()
        except sqlite3.OperationalError as error:
            raise EmbeddingContractError("INDEX_UPDATE_BUSY") from error
        return json.loads(row[0]) if row else None

    def pending(self):
        value = self._read()
        return value if value and value["phase"] != "idle" else None

    def read_stamp(self):
        value = self._read()
        if value and value["phase"] != "idle":
            raise EmbeddingContractError("INDEX_RECOVERY_REQUIRED")
        return value.get("revision") if value else None

    def assert_unchanged(self, stamp):
        if self.read_stamp() != stamp:
            raise EmbeddingContractError("INDEX_CHANGED_DURING_SEARCH")

    def assert_clean(self):
        if self.pending() is not None:
            raise EmbeddingContractError("INDEX_RECOVERY_REQUIRED")

    @contextmanager
    def lease(self):
        self.directory.mkdir(parents=True, exist_ok=True)
        with (self.directory / "writer.lock").open("a+b") as handle:
            handle.seek(0, 2)
            if handle.tell() == 0:
                handle.write(b"0")
                handle.flush()
            handle.seek(0)
            try:
                if os.name == "nt":
                    import msvcrt
                    msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as error:
                raise EmbeddingContractError("INDEX_UPDATE_BUSY") from error
            try:
                yield
            finally:
                handle.seek(0)
                if os.name == "nt":
                    msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    fcntl.flock(handle, fcntl.LOCK_UN)

    def _save(self, value):
        self.directory.mkdir(parents=True, exist_ok=True)
        with sqlite3.connect(self.path) as conn:
            conn.execute("PRAGMA synchronous=FULL")
            conn.execute("CREATE TABLE IF NOT EXISTS replacement (id INTEGER PRIMARY KEY, body TEXT NOT NULL)")
            conn.execute("INSERT OR REPLACE INTO replacement VALUES (1, ?)",
                         (json.dumps(value, ensure_ascii=False),))

    def begin(self, dataset, file_name, db_file_name, ids):
        self.assert_clean()
        if not ids or len(set(ids)) != len(ids):
            raise ValueError("Replacement requires unique, nonempty staged IDs")
        self._save(dict(phase="staging", dataset=dataset, file_name=file_name,
                        db_file_name=db_file_name, ids=list(ids), revision=uuid.uuid4().hex))

    def commit(self):
        value = self.pending()
        if value is None or value["phase"] != "staging":
            raise RuntimeError("No staged replacement")
        value["phase"] = "committing"
        self._save(value)

    def finish(self):
        if self.path.exists():
            value = self.pending() or {}
            scope = value.get("scope")
            if value.get("phase") in {"staging", "committing"}:
                scope = {"dataset": value["dataset"], "file": value["file_name"]}
            # The invalidation and idle revision are one durable transaction.
            # Unknown writers invalidate the whole projection, never silently skip it.
            with sqlite3.connect(self.path) as conn:
                conn.execute("PRAGMA synchronous=FULL")
                conn.execute("CREATE TABLE IF NOT EXISTS sparse_changes (scope TEXT PRIMARY KEY)")
                conn.execute("INSERT OR IGNORE INTO sparse_changes VALUES (?)", (json.dumps(scope),))
                conn.execute("UPDATE replacement SET body=? WHERE id=1", (
                    json.dumps(dict(phase="idle", revision=uuid.uuid4().hex)),))

    def sparse_changes(self):
        if not self.path.exists():
            return []
        with sqlite3.connect(self.path) as conn:
            if not conn.execute("SELECT 1 FROM sqlite_master WHERE name='sparse_changes'").fetchone():
                return []
            return [json.loads(row[0]) for row in conn.execute("SELECT scope FROM sparse_changes")]

    def publish_sparse(self, revision):
        with sqlite3.connect(self.path) as conn:
            conn.execute("PRAGMA synchronous=FULL")
            conn.execute("CREATE TABLE IF NOT EXISTS sparse_changes (scope TEXT PRIMARY KEY)")
            conn.execute("DELETE FROM sparse_changes")
            conn.execute("UPDATE replacement SET body=? WHERE id=1", (
                json.dumps(dict(phase="idle", revision=revision)),))

    def recover(self, client, adapter):
        """Caller holds the lease; repeat safely after any interrupted recovery."""
        value = self.pending()
        if value is None:
            return
        if value["phase"] in {"sparse_refresh", "sparse_dirty"}:
            from backend.sparse_index import rebuild_locked
            rebuild_locked(client, self, mode=value.get("mode", "bm25"),
                           vector_name=value.get("vector_name", "bm25_sparse"))
            return
        dataset, name, ids = value["dataset"], value["file_name"], value["ids"]
        if value["phase"] == "staging":
            discard_staged(client, self.collection, ids)
        elif value["phase"] == "committing":
            points = []
            for start in range(0, len(ids), 128):
                points.extend(client.retrieve(self.collection, ids=ids[start:start + 128],
                                              with_payload=True, with_vectors=False))
            if {str(point.id) for point in points} != set(ids) or any(
                (point.payload or {}).get("dataset_id") != dataset or
                (point.payload or {}).get("file_name") != name for point in points
            ):
                raise EmbeddingContractError("INDEX_RECOVERY_INCOMPLETE")
            retire_previous(client, self.collection, dataset, name, ids)
            adapter._sync_replace_file_lexical(dataset, name, points)
            adapter.db.clear_structured_rules(name)
        else:
            raise EmbeddingContractError("INDEX_RECOVERY_INCOMPLETE")
        # The source may have changed during the crash. Do not assert INDEXED
        # until normal ingestion has checked it again.
        adapter.db.update_document_status(dataset, value["db_file_name"], "PENDING", 0,
                                          last_error="Восстановлено после прерывания. Нужна проверка источника.")
        adapter.db.update_dataset_chunk_count(dataset)
        self.finish()


def recover_adapter(adapter):
    journal = ReplacementJournal.for_adapter(adapter)
    if journal.pending() is None:
        return
    from backend import qdrant_support as support
    with journal.lease():
        with closing(support.qdrant_client.QdrantClient(
            url=adapter.qdrant_url, timeout=60, check_compatibility=False,
            **support.qdrant_client_options(adapter.qdrant_url)
        )) as client:
            journal.recover(client, adapter)
