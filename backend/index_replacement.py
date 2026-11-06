"""Durable recovery for the Qdrant/FTS/catalog file replacement boundary."""
from contextlib import contextmanager, closing
import hashlib
import os
from pathlib import Path

from qdrant_client import models
from backend import native_core
from backend.interface import EmbeddingContractError
from backend.native_index_store import mutate


def retire_previous(client, collection: str, dataset: str, file_name: str, keep_ids: list[str]):
    if mutate("retire", collection, dataset_id=dataset, file_name=file_name, ids=keep_ids):
        return
    client.delete(collection_name=collection, wait=True, points_selector=models.FilterSelector(
        filter=models.Filter(must=[
            models.FieldCondition(key="dataset_id", match=models.MatchValue(value=dataset)),
            models.FieldCondition(key="file_name", match=models.MatchValue(value=file_name)),
        ], must_not=[models.HasIdCondition(has_id=keep_ids)])))


def discard_staged(client, collection: str, ids: list[str]):
    if ids and not mutate("discard", collection, ids=ids):
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

    def _journal(self, action, **arguments):
        try:
            return native_core.required('index_journal', request={
                'database': str(self.path.resolve()), 'action': action, 'arguments': arguments,
            })['value']
        except native_core.NativeCoreError as error:
            code = str(error)
            if code == 'INDEX_JOURNAL_INVALID':
                raise ValueError('Invalid replacement journal operation') from error
            raise EmbeddingContractError(code) from error

    def pending(self):
        value = self._journal('read')
        return value if value and value["phase"] != "idle" else None

    def read_stamp(self):
        value = self._journal('read')
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
        self._journal('store_sparse', value=value)

    def begin(self, dataset, file_name, db_file_name, ids):
        self._journal('begin', dataset=dataset, file_name=file_name, db_file_name=db_file_name, ids=list(ids))

    def commit(self):
        self._journal('commit')

    def finish(self):
        self._journal('finish')

    def sparse_changes(self):
        return self._journal('sparse_changes')

    def publish_sparse(self, revision):
        self._journal('publish_sparse', revision=revision)

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
