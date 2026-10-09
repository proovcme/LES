"""Production BM25 verified against a separate scalar oracle, without an LLM."""
from collections import Counter
from contextlib import closing
import math
import os
from pathlib import Path
from types import SimpleNamespace

import pytest
from qdrant_client import QdrantClient, models

from backend.index_replacement import ReplacementJournal
from backend.inference.bm25_sparse import _term_id, encode_bm25
from backend.inference.lexical_tokens import tokenize_current as tokenize
from backend.interface import EmbeddingContractError
from backend.sparse_index import ensure_current, encode_query, external_mutation, read_contract


def put(client, texts, start=0):
    points = []
    for index, text in enumerate(texts, start):
        terms = encode_bm25(text)
        points.append(models.PointStruct(id=index, vector={"dense": [1., 0.],
            "bm25_sparse": models.SparseVector(indices=list(terms), values=list(terms.values()))},
            payload={"text": text, "dataset_id": "а", "file_name": "Путь с пробелами.pdf",
                     "source_page": index + 1, "parent_id": "p"}))
    client.upsert("docs", points=points)


@pytest.fixture
def index(tmp_path):
    client = QdrantClient(":memory:")
    client.create_collection("docs", vectors_config={"dense": models.VectorParams(
        size=2, distance=models.Distance.COSINE)}, sparse_vectors_config={
        "bm25_sparse": models.SparseVectorParams(modifier=models.Modifier.IDF)})
    yield client, ReplacementJournal(tmp_path, "docs")
    client.close()


def scores(client, journal, query):
    from backend.sparse_index import search
    return dict(search(client, journal, query, limit=200))


def oracle(texts, query):
    documents = [Counter(tokenize(text)) for text in texts]
    average = sum(sum(row.values()) for row in documents) / len(documents)
    result = {}
    for index, row in enumerate(documents):
        score = 0.
        for term in set(tokenize(query)):
            tf = row[term]
            df = sum(term in document for document in documents)
            idf = math.log(1 + (len(documents) - df + .5) / (df + .5))
            score += idf * tf * 2.2 / (tf + 1.2 * (.25 + .75 * sum(row.values()) / average))
        if score:
            result[index] = score
    return result


def test_scores_match_full_bm25_and_keep_dense_payload_identity(index):
    client, journal = index
    texts = ["кабель кабель сечение", "кабель " + "документ " * 50,
             "кабель сечение ток", "заземление", "ПЕ", ""]
    put(client, texts)
    before = client.retrieve("docs", ids=list(range(len(texts))), with_vectors=True)
    contract = ensure_current(client, journal)
    assert contract["points"] == len(texts)
    assert contract["profile"]["average_length"] == sum(map(lambda t: len(tokenize(t)), texts)) / len(texts)
    assert scores(client, journal, "кабель кабель сечение") == pytest.approx(oracle(texts, "кабель сечение"), rel=1e-5)
    after = client.retrieve("docs", ids=list(range(len(texts))), with_vectors=True)
    for old, new in zip(before, after, strict=True):
        assert old.id == new.id and old.payload == new.payload
        assert old.vector["dense"] == new.vector["dense"]
    # Repeated reads neither change generation nor rewrite weights.
    assert ensure_current(client, journal) == contract


@pytest.fixture
def native_index(tmp_path):
    # QdrantClient local emulation retains deleted terms in IDF statistics.
    # This lifecycle test must exercise the actual server used by the product.
    executable = os.getenv("LES_TEST_NATIVE_QDRANT")
    if not executable:
        pytest.skip("Set LES_TEST_NATIVE_QDRANT for native IDF lifecycle verification")
    from backend.light_qdrant_runtime import LightQdrantRuntime
    runtime = LightQdrantRuntime(Path(executable), tmp_path)
    try:
        runtime.start()
        with closing(QdrantClient(url=runtime.url, api_key=runtime.api_key, check_compatibility=False)) as client:
            client.create_collection("docs", vectors_config={"dense": models.VectorParams(
                size=2, distance=models.Distance.COSINE)}, sparse_vectors_config={
                "bm25_sparse": models.SparseVectorParams(modifier=models.Modifier.IDF)})
            yield client, ReplacementJournal(tmp_path, "docs")
    finally:
        runtime.stop()


def test_add_replace_delete_recalculate_current_corpus_mean(native_index):
    client, journal = native_index
    texts = ["кабель", "кабель кабель"]
    put(client, texts)
    ensure_current(client, journal)
    for operation in ("add", "replace", "delete"):
        with external_mutation(journal, ids=[0, 2]):
            if operation == "add":
                texts.append("кабель " + "сечение " * 30)
                put(client, texts[-1:], 2)
            elif operation == "replace":
                texts[0] = "кабель сечение автомат"
                put(client, texts[:1])
            else:
                texts.pop()
                client.delete("docs", models.PointIdsList(points=[2]))
        with pytest.raises(EmbeddingContractError, match="INDEX_SPARSE_STALE"):
            encode_query(journal, "кабель")
        ensure_current(client, journal)
        assert scores(client, journal, "кабель") == pytest.approx(oracle(texts, "кабель"), rel=1e-5)


def test_interrupted_partial_reweight_is_blocked_then_recovers(index, monkeypatch):
    client, journal = index
    put(client, ["кабель " * (index % 4 + 1) for index in range(150)])
    from backend import bm25_store
    original = bm25_store.put
    calls = []
    def fail(*args, **kwargs):
        original(*args, **kwargs)
        calls.append(True)
        raise OSError("power failure")
    monkeypatch.setattr(bm25_store, "put", fail)
    with pytest.raises(OSError):
        ensure_current(client, journal)
    assert calls and journal.pending()["phase"] == "sparse_refresh"
    with pytest.raises(EmbeddingContractError, match="INDEX_RECOVERY_REQUIRED"):
        journal.read_stamp()
    monkeypatch.setattr(bm25_store, "put", original)
    with journal.lease():
        journal.recover(client, SimpleNamespace())
    assert read_contract(journal)["points"] == 150
    assert scores(client, journal, "кабель") == pytest.approx(
        oracle(["кабель " * (i % 4 + 1) for i in range(150)], "кабель"), rel=1e-5)


def test_rollback_to_legacy_then_bm25_is_explicit_and_repeatable(index):
    client, journal = index
    put(client, ["кабель кабель сечение", "заземление"])
    ensure_current(client, journal)
    ensure_current(client, journal, mode="tf")
    assert encode_query(journal, "кабель кабель")[_term_id("кабел")] == 2
    assert ensure_current(client, journal)["mode"] == "tf"
    ensure_current(client, journal, mode="bm25")
    assert scores(client, journal, "кабель кабель") == scores(client, journal, "кабель")


def test_empty_corpus_and_corrupt_contract_fail_closed(index):
    client, journal = index
    assert ensure_current(client, journal)["points"] == 0
    (journal.directory / "sparse.json").write_text('{"schema":"future"}')
    with pytest.raises(EmbeddingContractError, match="INDEX_SPARSE_CONTRACT_INVALID"):
        ensure_current(client, journal)


def test_explicit_rollback_after_interrupted_migration(index):
    client, journal = index
    put(client, ["кабель кабель"])
    journal._save(dict(phase="sparse_refresh", mode="bm25", vector_name="bm25_sparse", revision="interrupted"))
    assert ensure_current(client, journal, mode="tf")["mode"] == "tf"
    assert set(encode_query(journal, "кабель кабель").values()) == {2.}


def test_migration_respects_writer_lock(index):
    client, journal = index
    with journal.lease():
        with pytest.raises(EmbeddingContractError, match="INDEX_UPDATE_BUSY"):
            ensure_current(client, journal)


def test_idf_modifier_cannot_be_applied_twice(index):
    client, journal = index
    put(client, ["кабель", "заземление"])
    original = ensure_current(client, journal)
    client.update_collection("docs", sparse_vectors_config={
        "bm25_sparse": models.SparseVectorParams(modifier=models.Modifier.IDF)})
    repaired = ensure_current(client, journal)
    # Qdrant modifiers no longer affect dynamic BM25; no rebuild or double IDF.
    assert repaired["revision"] == original["revision"]
    assert scores(client, journal, "кабель") == pytest.approx(oracle(["кабель", "заземление"], "кабель"), rel=1e-5)


def test_failure_publishing_contract_recovers(index, monkeypatch):
    from backend import sparse_index
    client, journal = index
    put(client, ["кабель"])
    save = sparse_index._save_contract
    def fail(*args): raise OSError("disk full")
    monkeypatch.setattr(sparse_index, "_save_contract", fail)
    with pytest.raises(OSError, match="disk full"):
        ensure_current(client, journal)
    with pytest.raises(EmbeddingContractError, match="INDEX_RECOVERY_REQUIRED"):
        journal.read_stamp()
    monkeypatch.setattr(sparse_index, "_save_contract", save)
    assert ensure_current(client, journal)["mode"] == "bm25"


@pytest.mark.parametrize("text", ["Кабель\u00a0сечение", "Кабель\tсечение", "КАБЕЛЬ\nсечение", "кабель   сечение"])
def test_unicode_case_and_whitespace(index, text):
    client, journal = index
    put(client, [text])
    ensure_current(client, journal)
    assert scores(client, journal, "кабель сечение") == pytest.approx(oracle([text], "кабель сечение"), rel=1e-5)
