"""Versioned lexical projection with incremental BM25 statistics and recovery."""
from contextlib import contextmanager
from dataclasses import asdict
import logging
import uuid

from qdrant_client import models
from backend import bm25_store, sparse_legacy
from backend.inference.bm25_weighted import BM25Profile
from backend.inference.lexical_tokens import CURRENT_TOKENIZER
from backend.interface import EmbeddingContractError
from backend.sparse_legacy import read_contract, _save_contract, _physical_collection

LOG = logging.getLogger(__name__)
STORAGE = "sqlite-postings"


def _points(client, journal, scope):
    if scope and "ids" in scope:
        ids = scope["ids"]
        for start in range(0, len(ids), 128):
            yield client.retrieve(journal.collection, ids=ids[start:start + 128],
                                  with_payload=True, with_vectors=False)
        return
    conditions = []
    if scope:
        match = (models.MatchAny(any=scope["datasets"]) if "datasets" in scope
                 else models.MatchValue(value=scope["dataset"]))
        conditions.append(models.FieldCondition(key="dataset_id", match=match))
        if "file" in scope:
            conditions.append(models.FieldCondition(key="file_name", match=models.MatchValue(value=scope["file"])))
    offset = None
    while True:
        points, offset = client.scroll(journal.collection, offset=offset, limit=256,
            scroll_filter=models.Filter(must=conditions) if conditions else None,
            with_payload=["text", "dataset_id", "file_name", "node_role", "ancestor_ids"], with_vectors=False)
        yield points
        if offset is None:
            break


def rebuild_locked(client, journal, *, mode="bm25", vector_name="bm25_sparse", incremental=False):
    """Caller owns the lease. A crash keeps reads blocked until recovery completes."""
    if mode == "tf":
        return sparse_legacy.rebuild_locked(client, journal, mode=mode, vector_name=vector_name)
    if mode != "bm25":
        raise ValueError("Unknown sparse mode")
    scopes = journal.sparse_changes() if incremental else []
    full = not scopes or any(scope is None for scope in scopes)
    journal._save(dict(phase="sparse_refresh", mode=mode, vector_name=vector_name,
                       revision=uuid.uuid4().hex))
    revision = uuid.uuid4().hex
    touched = 0
    with bm25_store.connect(journal, create=True) as db, db:
        db.execute("BEGIN IMMEDIATE")
        if full:
            bm25_store.reset(db)
        for scope in ([None] if full else scopes):
            if scope:
                bm25_store.clear_scope(db, scope)
            for points in _points(client, journal, scope):
                bm25_store.put(db, points)
                touched += len(points)
                LOG.info("[BM25] %s lexical records: %s", "Migrating" if full else "Updating", touched)
        db.execute("DELETE FROM frequencies WHERE df=0")
        db.execute("UPDATE corpus SET revision=? WHERE id=1", (revision,))
        count, length, _ = db.execute("SELECT n, length, revision FROM corpus WHERE id=1").fetchone()
        if count != int(client.count(journal.collection, exact=True).count):
            raise EmbeddingContractError("INDEX_SPARSE_STALE")
    profile = BM25Profile(length / count if count and length else 1.)
    value = dict(schema="les.sparse-index.v2", mode=mode, storage=STORAGE,
        vector_name=vector_name, physical_collection=_physical_collection(client, journal.collection),
        tokenizer=CURRENT_TOKENIZER, idf="exact-corpus", points=count, total_length=length,
        profile=asdict(profile), revision=revision)
    _save_contract(journal, value)
    journal.publish_sparse(revision)
    LOG.info("[BM25] Published %s fragments; read %s changed records", count, touched)
    return value


def ensure_current(client, journal, *, vector_name="bm25_sparse", mode=None):
    with journal.lease():
        contract = read_contract(journal)
        pending = journal.pending()
        target = mode or (pending or contract or {}).get("mode", "bm25")
        if pending:
            if pending["phase"] not in {"sparse_refresh", "sparse_dirty"}:
                raise EmbeddingContractError("INDEX_RECOVERY_REQUIRED")
            return rebuild_locked(client, journal, mode=target, vector_name=vector_name)
        if target == "tf":
            # Avoid acquiring the lease twice; legacy writes are only for explicit rollback.
            if contract and contract["mode"] == "tf" and contract["revision"] == journal.read_stamp():
                if (contract["points"] == int(client.count(journal.collection, exact=True).count)
                        and contract.get("physical_collection") == _physical_collection(client, journal.collection)
                        and client.get_collection(journal.collection).config.params.sparse_vectors[vector_name].modifier
                            == models.Modifier.IDF):
                    return contract
            return rebuild_locked(client, journal, mode="tf", vector_name=vector_name)
        if target != "bm25":
            raise ValueError("Unknown sparse mode")
        valid = (contract and contract.get("storage") == STORAGE
            and contract["tokenizer"] == CURRENT_TOKENIZER and contract["vector_name"] == vector_name
            and contract.get("physical_collection") == _physical_collection(client, journal.collection)
            and (journal.directory / "bm25.sqlite").is_file())
        if valid:
            count, length, revision = bm25_store.metadata(journal)
            valid = (count == contract["points"] and length == contract["total_length"]
                     and revision == contract["revision"])
            if valid and revision == journal.read_stamp():
                if count == int(client.count(journal.collection, exact=True).count):
                    return contract
                valid = False  # Untracked change: full repair.
        return rebuild_locked(client, journal, vector_name=vector_name, incremental=bool(valid))


def encode_query(journal, text):
    contract = read_contract(journal) or {}
    if contract.get("storage") == STORAGE:
        # Dynamic scores cannot be encoded as one vector. Never query stale legacy weights.
        if contract["revision"] != journal.read_stamp():
            raise EmbeddingContractError("INDEX_SPARSE_STALE")
        raise EmbeddingContractError("INDEX_SPARSE_QUERY_REQUIRES_POSTINGS")
    return sparse_legacy.encode_query(journal, text)


def search(client, journal, text, *, limit=24, **filters):
    if (read_contract(journal) or {}).get("storage") == STORAGE:
        return bm25_store.search(journal, text, limit=limit, **filters)
    terms = encode_query(journal, text)
    stamp = journal.read_stamp()
    conditions = []
    for key, name in (("dataset_ids", "dataset_id"), ("doc_filter", "file_name"),
                      ("node_roles", "node_role"), ("ancestor_ids", "ancestor_ids")):
        if filters.get(key):
            conditions.append(models.FieldCondition(key=name, match=models.MatchAny(any=filters[key])))
    result = client.query_points(journal.collection, using="bm25_sparse", limit=limit,
        query_filter=models.Filter(must=conditions) if conditions else None,
        query=models.SparseVector(indices=list(terms), values=list(terms.values())))
    journal.assert_unchanged(stamp)
    return [(point.id, point.score) for point in result.points]


@contextmanager
def external_mutation(journal, *, dataset=None, datasets=None, file=None, ids=None):
    """Persist precise scope before writes; unknown scopes rebuild safely."""
    scope = {"ids": list(ids)} if ids is not None else (
        {"dataset": dataset, **({"file": file} if file is not None else {})} if dataset is not None else None)
    if datasets is not None:
        scope = {"datasets": list(datasets)}
    with journal.lease():
        journal.assert_clean()
        contract = read_contract(journal)
        journal._save(dict(phase="sparse_dirty", mode=(contract or {}).get("mode", "bm25"), scope=scope,
                           vector_name=(contract or {}).get("vector_name", "bm25_sparse"),
                           revision=uuid.uuid4().hex))
        try:
            yield
        finally:
            journal.finish()
