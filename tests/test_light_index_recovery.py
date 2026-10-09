"""Real persistent index recovery and vector-space boundary regressions."""
import asyncio
import os
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
import uuid

import pytest
from qdrant_client import QdrantClient, models

from backend.embedding_client import EmbedClient
from backend.index_replacement import ReplacementJournal, retire_previous
from backend.interface import EmbeddingContractError
from backend.qdrant_integrity import QdrantIntegrity
from backend.qdrant_retrieval import QdrantRetrieval
from proxy.services.lexical_index_service import LexicalIndex


def _point(id, text, dataset="ds", name="source.txt"):
    return models.PointStruct(id=id, vector=[1., 0.],
        payload=dict(text=text, dataset_id=dataset, file_name=name))


@pytest.mark.parametrize("phase", ["staging", "partial", "commit", "retired", "lexical"])
def test_process_exit_recovers_one_generation_and_fts(tmp_path, phase):
    """os._exit bypasses Python cleanup and releases the writer's OS lease."""
    ids = [str(uuid.uuid4()) for _ in range(4)]
    client = QdrantClient(path=str(tmp_path / "vectors"))
    client.create_collection("docs", vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
    client.upsert("docs", points=[_point(ids[0], "old"), _point(ids[3], "other", dataset="unrelated")])
    client.close()
    lexical = LexicalIndex(str(tmp_path / "fts.sqlite"))
    lexical.upsert_chunks("docs", [
        dict(point_id=ids[0], dataset_id="ds", doc_name="source.txt", text="old"),
        dict(point_id=ids[3], dataset_id="unrelated", doc_name="source.txt", text="other")])
    script = """
import os, sys
from pathlib import Path
from qdrant_client import QdrantClient, models
from backend.index_replacement import ReplacementJournal, retire_previous
from proxy.services.lexical_index_service import LexicalIndex
root, phase, *ids = sys.argv[1:]
root=Path(root)
journal=ReplacementJournal(root, "docs")
with journal.lease():
    client=QdrantClient(path=str(root/"vectors"))
    journal.begin("ds", "source.txt", "source.txt", ids[1:3])
    if phase=="staging": os._exit(71)
    points=[models.PointStruct(id=id,vector=[1.,0.],payload=dict(
        text="new",dataset_id="ds",file_name="source.txt")) for id in ids[1:3]]
    client.upsert("docs",points=points[:1] if phase=="partial" else points)
    if phase=="partial": os._exit(71)
    journal.commit()
    if phase=="commit": os._exit(71)
    retire_previous(client,"docs","ds","source.txt",ids[1:3])
    if phase=="retired": os._exit(71)
    LexicalIndex(str(root/"fts.sqlite")).replace_file("docs",dataset_id="ds",
        doc_name="source.txt",rows=[dict(point_id=id,dataset_id="ds",
        doc_name="source.txt",text="new") for id in ids[1:3]])
    os._exit(71)
"""
    result = subprocess.run([sys.executable, "-c", script, str(tmp_path), phase, *ids],
                            cwd=Path(__file__).resolve().parents[1], capture_output=True, timeout=40)
    assert result.returncode == 71, result.stderr.decode(errors="replace")
    journal = ReplacementJournal(tmp_path, "docs")
    with pytest.raises(EmbeddingContractError, match="INDEX_RECOVERY_REQUIRED"):
        journal.assert_clean()
    statuses = []
    adapter = SimpleNamespace(
        db=SimpleNamespace(update_document_status=lambda *a, **k: statuses.append(a),
                           update_dataset_chunk_count=lambda *a: None, clear_structured_rules=lambda *a: None),
        _sync_replace_file_lexical=lambda ds, name, points: lexical.replace_file("docs",
            dataset_id=ds, doc_name=name, rows=QdrantIntegrity._lexical_rows_from_points(points)))
    client = QdrantClient(path=str(tmp_path / "vectors"))
    with journal.lease():
        journal.recover(client, adapter)
        journal.recover(client, adapter)
    journal.assert_clean()
    expected = {ids[0], ids[3]} if phase in {"staging", "partial"} else {ids[1], ids[2], ids[3]}
    assert {str(p.id) for p in client.scroll("docs", limit=10)[0]} == expected
    with lexical.connect() as conn:
        assert {r[0] for r in conn.execute("SELECT point_id FROM lexical_chunks")} == expected
    assert len(statuses) == 1
    assert statuses[0][2] == "PENDING"
    client.close()


def test_missing_committed_points_preserves_journal_and_old_evidence(tmp_path):
    client = QdrantClient(":memory:")
    client.create_collection("docs", vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
    old, new = [str(uuid.uuid4()) for _ in range(2)]
    client.upsert("docs", points=[_point(old, "old")])
    journal = ReplacementJournal(tmp_path, "docs")
    with journal.lease():
        journal.begin("ds", "source.txt", "source.txt", [new])
        journal.commit()
        with pytest.raises(EmbeddingContractError, match="INDEX_RECOVERY_INCOMPLETE"):
            journal.recover(client, SimpleNamespace())
    assert journal.pending()["phase"] == "committing"
    assert client.retrieve("docs", ids=[old])
    client.close()


def test_writer_lease_and_search_revision(tmp_path):
    journal = ReplacementJournal(tmp_path, "docs")
    stamp = journal.read_stamp()
    with journal.lease():
        with pytest.raises(EmbeddingContractError, match="INDEX_UPDATE_BUSY"):
            with ReplacementJournal(tmp_path, "docs").lease():
                pytest.fail("second writer acquired lease")
        journal.begin("ds", "source", "source", [str(uuid.uuid4())])
        journal.finish()
    with pytest.raises(EmbeddingContractError, match="INDEX_CHANGED_DURING_SEARCH"):
        journal.assert_unchanged(stamp)


def test_startup_recovery_closes_qdrant_without_context_manager(tmp_path, monkeypatch):
    from backend import qdrant_support as support
    from backend.index_replacement import recover_adapter
    journal = ReplacementJournal(tmp_path, "docs")
    with journal.lease():
        journal.begin("ds", "source", "source", [str(uuid.uuid4())])
    calls = []
    class Client:
        def __init__(self, **kwargs): pass
        def delete(self, **kwargs): calls.append("delete")
        def close(self): calls.append("close")
    monkeypatch.setattr(support.qdrant_client, "QdrantClient", Client)
    monkeypatch.setattr(support, "qdrant_client_options", lambda url: {})
    adapter = SimpleNamespace(content_dir=tmp_path, collection_name="docs", qdrant_url="http://localhost",
        db=SimpleNamespace(update_document_status=lambda *a, **kw: calls.append("pending"),
                           update_dataset_chunk_count=lambda *a: calls.append("count")))
    recover_adapter(adapter)
    assert calls == ["delete", "pending", "count", "close"]
    journal.assert_clean()


def test_lexical_replace_rolls_back_both_delete_and_partial_insert(tmp_path):
    lexical = LexicalIndex(str(tmp_path / "fts.sqlite"))
    row = dict(point_id="old", dataset_id="ds", doc_name="source", text="old")
    lexical.upsert_chunks("docs", [row])
    bad = {**row, "point_id": "bad", "chunk_ord": object()}
    with pytest.raises(Exception):
        lexical.replace_file("docs", dataset_id="ds", doc_name="source",
                             rows=[{**row, "point_id": "new"}, bad])
    with lexical.connect() as conn:
        assert [r[0] for r in conn.execute("SELECT point_id FROM lexical_chunks")] == ["old"]


def test_embedding_binding_frozen_for_file_and_checked_before_network():
    calls = []
    connection = SimpleNamespace(model_id="bge-m3", revision_id="first")
    resolver = SimpleNamespace(resolve=lambda *a, **k: connection)
    class Transport:
        async def embed(self, conn, texts):
            calls.append(conn.revision_id)
            return SimpleNamespace(model_id=conn.model_id, vectors=[[1., 0.] for _ in texts])
    client = EmbedClient("http://unused", model="bge-m3", connection_mode="active",
                         connection_resolver=resolver, connection_transport=Transport())
    descriptor = dict(model_id="BAAI/bge-m3", vector_size="2")
    frozen = client.for_index(descriptor)
    connection = SimpleNamespace(model_id="different-model", revision_id="second")
    frozen.encode_sync(["first batch"])
    frozen.encode_sync(["second batch"])
    assert calls == ["first", "first"]
    with pytest.raises(EmbeddingContractError, match="EMBEDDING_INDEX_MODEL_MISMATCH"):
        client.for_index(descriptor)
    assert len(calls) == 2
    with pytest.raises(EmbeddingContractError, match="EMBEDDING_INDEX_DIMENSION_MISMATCH"):
        frozen.for_index({**descriptor, "vector_size": "3"}).encode_sync(["bad size"])


@pytest.mark.asyncio
async def test_hierarchy_embeds_once_and_retains_all_three_search_passes(tmp_path, monkeypatch):
    from backend import qdrant_support as support
    monkeypatch.setattr(support, "_qdrant_schema_mode", lambda: "named")
    queries, embedded = [], []
    async def encode(texts, **kwargs):
        embedded.append(texts)
        return [[1., 0.]]
    async def query(**kwargs):
        queries.append(kwargs)
        text = "navigation" if len(queries) == 2 else "evidence"
        return SimpleNamespace(points=[SimpleNamespace(id=str(len(queries)), score=1.,
            payload=dict(text=text, node_id="parent", doc_id=text, file_name="source"))])
    adapter = SimpleNamespace(content_dir=tmp_path, collection_name="docs",
        _ensure_collection=lambda: asyncio.sleep(0), _assert_dense_index_contract=lambda: None,
        embed=SimpleNamespace(encode_async=encode), aclient=SimpleNamespace(query_points=query))
    adapter.retrieve_native_hybrid = lambda *a, **kw: QdrantRetrieval.retrieve_native_hybrid(adapter, *a, **kw)
    result = await QdrantRetrieval.retrieve_native_hierarchical(adapter, "проверка документа")
    assert len(embedded) == 1
    assert len(queries) == 3
    assert result and all(chunk.content == "evidence" for chunk in result)
    assert all(query["prefetch"][0].query == [1., 0.] for query in queries)


def test_citation_number_without_available_source_is_invalid():
    from proxy.services.evidence_packet_service import verify_answer_source_labels
    result = verify_answer_source_labels("Ответ [Источник 1]", [])
    assert result["status"] == "invalid_labels"
    assert result["claim_verification"] == "not_performed"


@pytest.mark.asyncio
@pytest.mark.parametrize("finish", [False, True])
async def test_replacement_during_rerank_never_returns_mixed_evidence(tmp_path, finish):
    from tests.test_retrieval_service import FakeBackend
    from proxy.services.retrieval_service import retrieve_chat_chunks
    backend = FakeBackend()
    backend.content_dir = tmp_path
    backend._ensure_collection = lambda: asyncio.sleep(0)
    journal = ReplacementJournal.for_adapter(backend)
    class ConcurrentReranker:
        def __init__(self, **kwargs): pass
        async def rerank(self, query, chunks, **kwargs):
            with journal.lease():
                journal.begin("ds-1", "file", "file", [str(uuid.uuid4())])
                if finish: journal.finish()
            return [SimpleNamespace(metadata={"_idx": i}, score=1.) for i in range(len(chunks))]
    result = await retrieve_chat_chunks(question="документ", dataset_ids=["ds-1"],
        rag_backend=backend, return_trace=True, reranker_enabled=True, reranker_available=True,
        reranker_cls=ConcurrentReranker, mlx_url="",
        logger=SimpleNamespace(info=lambda *a: None, warning=lambda *a: None))
    assert result.trace.error_code == ("INDEX_CHANGED_DURING_SEARCH" if finish else "INDEX_RECOVERY_REQUIRED")
    assert result.trace.status == "blocked"
    assert not result.chunks


@pytest.mark.asyncio
@pytest.mark.parametrize("error,code", [
    ("ROLE_BINDING_MISSING: embeddings", "ROLE_BINDING_MISSING"),
    ("UPSTREAM_REQUEST_FAILED: ReadTimeout", "UPSTREAM_TIMEOUT"),
    ("UPSTREAM_HTTP_ERROR: 404", "UPSTREAM_MODEL_NOT_FOUND"),
])
async def test_search_model_failure_is_actionable_and_never_falls_back(error, code):
    from tests.test_retrieval_service import FakeBackend
    from proxy.services.retrieval_service import retrieve_chat_chunks
    backend = FakeBackend()
    async def broken(*args, **kwargs):
        raise RuntimeError(error)
    backend.retrieve_native_hybrid = broken
    result = await retrieve_chat_chunks(question="вопрос", dataset_ids=["ds-1"],
        rag_backend=backend, return_trace=True, reranker_enabled=False, reranker_available=False, reranker_cls=None,
        mlx_url="", logger=SimpleNamespace(info=lambda *a: None, warning=lambda *a: None))
    assert result.trace.status == "blocked"
    assert result.trace.error_code == code
    assert not result.chunks
    assert len(result.trace.quality_detail) > 40
    assert "UPSTREAM" not in result.trace.quality_detail
