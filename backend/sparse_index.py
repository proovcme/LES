"""Recoverable, model-free BM25 projection of the current Qdrant corpus.

The replacement journal is the publication fence. Reweighting only touches the
named sparse vector; point identities, payloads and dense vectors stay intact.
"""
from contextlib import closing, contextmanager
from dataclasses import asdict
from functools import lru_cache
import json
import logging
import math
import os
import sqlite3
import uuid

from qdrant_client import models
from backend.inference.bm25_sparse import encode_bm25
from backend.inference.bm25_weighted import BM25Profile
from backend.interface import EmbeddingContractError

LOG = logging.getLogger(__name__)
SCHEMA = "les.sparse-index.v1"


def _physical_collection(client, name):
    return next((entry.collection_name for entry in client.get_aliases().aliases
                 if entry.alias_name == name), name)


def read_contract(journal):
    path = journal.directory / "sparse.json"
    if not path.exists():
        return None
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
        if (value["schema"] != SCHEMA or value["mode"] not in {"bm25", "tf"}
                or value["tokenizer"] != "les.lexical.v1"
                or value["idf"] != ("exact-corpus" if value["mode"] == "bm25" else "qdrant")):
            raise ValueError("Unknown sparse contract")
        if value["mode"] == "bm25":
            BM25Profile(**value["profile"])
        return value
    except (ValueError, KeyError, TypeError) as error:
        raise EmbeddingContractError("INDEX_SPARSE_CONTRACT_INVALID") from error


def _save_contract(journal, value):
    path = journal.directory / "sparse.json"
    temporary = path.with_suffix(".tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def rebuild_locked(client, journal, *, mode="bm25", vector_name="bm25_sparse"):
    """Caller owns journal.lease(); interrupted writes remain unreadable.

    Two bounded passes through a disk spool avoid keeping the corpus in RAM.
    Mean length is recomputed over every point, including hierarchy navigation,
    IDF uses the same live population, without Qdrant's delayed deletion stats.
    Empty text has length zero.
    """
    if mode not in {"bm25", "tf"}:
        raise ValueError("Unknown sparse mode")
    info = client.get_collection(journal.collection)
    config = info.config.params.sparse_vectors or {}
    if vector_name not in config:
        raise EmbeddingContractError("INDEX_SPARSE_CONTRACT_INVALID")
    journal.directory.mkdir(parents=True, exist_ok=True)
    # Mark before any vector write, also when re-entering after a crash.
    journal._save(dict(phase="sparse_refresh", mode=mode, vector_name=vector_name,
                       revision=uuid.uuid4().hex))
    spool = journal.directory / "sparse-spool.sqlite"
    count = total_length = 0
    LOG.info("[BM25] Preparing lexical weights for %s", journal.collection)
    with closing(sqlite3.connect(spool)) as db:
        db.execute("DROP TABLE IF EXISTS terms")
        db.execute("CREATE TABLE terms (id TEXT PRIMARY KEY, body TEXT, length INTEGER)")
        db.execute("DROP TABLE IF EXISTS frequencies")
        db.execute("CREATE TABLE frequencies (term INTEGER PRIMARY KEY, df INTEGER NOT NULL)")
        offset = None
        while True:
            points, offset = client.scroll(journal.collection, limit=128, offset=offset,
                                           with_payload=["text"], with_vectors=False)
            for point in points:
                terms = encode_bm25(str((point.payload or {}).get("text") or ""))
                length = int(sum(terms.values()))
                db.execute("INSERT INTO terms VALUES (?, ?, ?)",
                           (json.dumps(point.id), json.dumps(terms), length))
                db.executemany("INSERT INTO frequencies VALUES (?, 1) ON CONFLICT(term) DO UPDATE SET df=df+1",
                               ((term,) for term in terms))
                count += 1
                total_length += length
            db.commit()
            if offset is None:
                break
        profile = BM25Profile(total_length / count if total_length and count else 1.0)
        modifier = models.Modifier.NONE if mode == "bm25" else models.Modifier.IDF
        if config[vector_name].modifier != modifier:
            client.update_collection(journal.collection, sparse_vectors_config={
                vector_name: models.SparseVectorParams(modifier=modifier)})

        @lru_cache(maxsize=8192)
        def inverse_frequency(term):
            df = db.execute("SELECT df FROM frequencies WHERE term=?", (term,)).fetchone()[0]
            return math.log(1 + (count - df + .5) / (df + .5))

        cursor = db.execute("SELECT id, body, length FROM terms ORDER BY id")
        updated = 0
        while rows := cursor.fetchmany(128):
            batch = []
            for point_id, body, length in rows:
                terms = {int(key): float(value) for key, value in json.loads(body).items()}
                if mode == "bm25":
                    norm = profile.k1 * (1 - profile.b + profile.b * length / profile.average_length)
                    terms = {term: inverse_frequency(term) * (profile.k1 + 1) * tf / (tf + norm)
                             for term, tf in terms.items()}
                batch.append(models.PointVectors(id=json.loads(point_id), vector={
                    vector_name: models.SparseVector(indices=list(terms), values=list(terms.values()))}))
            client.update_vectors(journal.collection, points=batch, wait=True)
            updated += len(batch)
            LOG.info("[BM25] Lexical weights %s/%s", updated, count)
    # Publication happens only after Qdrant acknowledged every batch.
    revision = uuid.uuid4().hex
    value = dict(schema=SCHEMA, mode=mode, vector_name=vector_name,
                 physical_collection=_physical_collection(client, journal.collection),
                 tokenizer="les.lexical.v1", idf="exact-corpus" if mode == "bm25" else "qdrant", points=count,
                 total_length=total_length, profile=asdict(profile), revision=revision)
    _save_contract(journal, value)
    journal._save(dict(phase="idle", revision=revision))
    spool.unlink(missing_ok=True)
    return value


def ensure_current(client, journal, *, vector_name="bm25_sparse", mode=None):
    """Refresh once after a corpus change, before its first hybrid query."""
    with journal.lease():
        pending = journal.pending()
        if pending:
            if pending["phase"] not in {"sparse_refresh", "sparse_dirty"}:
                raise EmbeddingContractError("INDEX_RECOVERY_REQUIRED")
            recovered = rebuild_locked(client, journal, mode=pending.get("mode", "bm25"),
                                       vector_name=pending.get("vector_name", vector_name))
            if mode is None or recovered["mode"] == mode:
                return recovered
        stamp = journal.read_stamp()
        contract = read_contract(journal)
        target = mode or (contract["mode"] if contract else "bm25")
        count = int(client.count(journal.collection, exact=True).count)
        config = client.get_collection(journal.collection).config.params.sparse_vectors or {}
        expected_modifier = models.Modifier.NONE if target == "bm25" else models.Modifier.IDF
        if (contract and contract["revision"] == stamp and contract["mode"] == target
                and contract["vector_name"] == vector_name and contract["points"] == count
                and contract.get("physical_collection") == _physical_collection(client, journal.collection)
                and vector_name in config and config[vector_name].modifier == expected_modifier):
            return contract
        return rebuild_locked(client, journal, mode=target, vector_name=vector_name)


def encode_query(journal, text):
    contract = read_contract(journal)
    if not contract or contract["revision"] != journal.read_stamp():
        raise EmbeddingContractError("INDEX_SPARSE_STALE")
    if contract["mode"] == "bm25":
        return BM25Profile(**contract["profile"]).query(text)
    return encode_bm25(text)


@contextmanager
def external_mutation(journal):
    """Fence non-ingestion writes, invalidating cached weights even on failure."""
    with journal.lease():
        journal.assert_clean()
        contract = read_contract(journal)
        journal._save(dict(phase="sparse_dirty", mode=(contract or {}).get("mode", "bm25"),
                           vector_name=(contract or {}).get("vector_name", "bm25_sparse"),
                           revision=uuid.uuid4().hex))
        try:
            yield
        finally:
            journal.finish()
