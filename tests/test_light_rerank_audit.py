"""Failure cases found in the optional reranker and retrieval audit."""
import math
import sys
from types import SimpleNamespace

import pytest

from backend import local_reranker
from proxy.services.dataset_memory_service import _topic_match_score
from proxy.services.public_error_service import public_error_payload
from tests.test_retrieval_service import FakeBackend
from proxy.services.retrieval_service import retrieve_chat_chunks


@pytest.mark.parametrize("text,hit", [("заземление шкафов", False), ("раздел ОВ", True),
                                      ("ОВ-1", True), ("новый", False), ("éовé", False)])
def test_short_topic_codes_are_whole_unicode_tokens(text, hit):
    score, _ = _topic_match_score(text.casefold(), ("ов",))
    assert bool(score) is hit


@pytest.mark.parametrize("detail,code", [
    ("ROLE_BINDING_MISSING: answer", "ROLE_BINDING_MISSING"),
    ("UPSTREAM_REQUEST_FAILED: ReadTimeout", "UPSTREAM_TIMEOUT"),
    ("UPSTREAM_REQUEST_FAILED: ConnectError", "UPSTREAM_UNREACHABLE"),
    ("UPSTREAM_HTTP_ERROR: 404", "UPSTREAM_MODEL_NOT_FOUND"),
    ("UPSTREAM_HTTP_ERROR: 401", "UPSTREAM_AUTH_FAILED"),
    ("UPSTREAM_HTTP_ERROR: 429", "UPSTREAM_RATE_LIMITED"),
])
def test_actionable_model_errors(detail, code):
    error = public_error_payload(status_code=502, detail=detail)
    assert error["code"] == code
    assert len(error["detail"]) > 45
    assert "UPSTREAM" not in error["detail"]


@pytest.mark.asyncio
@pytest.mark.parametrize("items", [[], [(0, math.nan)], [(0, math.inf)], [(True, 1)],
                                  [(999, 1)], [(0, 1), (0, 2)]])
async def test_invalid_reranker_never_claims_applied(items):
    class Broken:
        def __init__(self, **kwargs):
            pass

        async def rerank(self, *args, **kwargs):
            return [SimpleNamespace(metadata={"_idx": idx}, score=score) for idx, score in items]

    result = await retrieve_chat_chunks(question="audit", dataset_ids=["ds-1"],
        rag_backend=FakeBackend(), reranker_enabled=True, reranker_available=True,
        reranker_cls=Broken, mlx_url="", return_trace=True,
        logger=SimpleNamespace(info=lambda *a: None, warning=lambda *a: None))
    assert result.trace.status == "blocked"
    assert result.trace.error_code == "reranker_failed"
    assert not result.chunks


def test_worker_timeout_kills_owned_process_and_can_restart(tmp_path, monkeypatch):
    script = tmp_path / "worker.py"
    script.write_text('import sys,time\nfor line in sys.stdin: time.sleep(30)\n')
    worker = local_reranker.Worker(sys.executable, "unused", "cpu")
    import subprocess
    def start():
        worker.process = subprocess.Popen([sys.executable, str(script)], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, text=True, encoding="utf-8")
    monkeypatch.setattr(worker, "start", start)
    with pytest.raises(TimeoutError, match="RERANK_TIMEOUT"):
        worker.score([["a", "b"]], timeout=0.1, batch_size=1)
    assert worker.process is None
    with pytest.raises(TimeoutError, match="RERANK_TIMEOUT"):
        worker.score([["a", "b"]], timeout=0.1, batch_size=1)
    assert worker.process is None


def test_missing_weights_do_not_trigger_download(tmp_path, monkeypatch):
    monkeypatch.setenv("HF_HUB_CACHE", str(tmp_path))
    monkeypatch.setenv("RERANK_MODEL", "absent/model")
    assert local_reranker.model_snapshot("absent/model") is None
    assert local_reranker.readiness()["available"] is False
    assert list(tmp_path.iterdir()) == []


def test_context_drops_complete_sources_and_exposes_only_visible_proof():
    from proxy.services.chat_evidence_context import source_context_blocks, model_visible_source_map
    from proxy.services.context_governor_service import ContextCandidate, ContextKind, ContextObject, ContextGovernor
    from tests.test_context_governor_service import _preset
    text = '[Источник 1 | first.pdf]:\nПервый абзац.\n\nВторой абзац.\n\n[Источник 2 | second.pdf]:\n' + 'x' * 800
    blocks = source_context_blocks(text)
    assert len(blocks) == 2
    assert 'Второй абзац' in blocks[0]
    packet = ContextGovernor(_preset(limit=300, generation=0, safety=0), estimate_tokens=len).pack([
        ContextCandidate(ContextKind.EVIDENCE, tuple(ContextObject(str(i), block) for i,block in enumerate(blocks)))])
    sources=[{'index':1,'label':'Источник 1'},{'index':2,'label':'Источник 2'}]
    assert model_visible_source_map(packet,sources) == sources[:1]
    assert len(sources) == 2


def test_direct_rerank_rejects_unbounded_or_nonfinite_input():
    from proxy.routers.rerank import RerankRequest
    from pydantic import ValidationError
    with pytest.raises(ValidationError):
        RerankRequest(query='q', chunks=[{'text':'a'}]*65)
    with pytest.raises(ValidationError):
        RerankRequest(query='q', chunks=[{'text':'a','score':float('nan')}])


@pytest.mark.parametrize('failure', ['embedding', 'upsert'])
def test_reindex_failure_keeps_previous_generation(tmp_path, monkeypatch, failure):
    from backend import qdrant_support as support
    from backend.qdrant_adapter import QdrantLlamaIndexAdapter
    from tests.test_qdrant_adapter_parse import StatusTrackingDB
    folder=tmp_path/'ds-1';folder.mkdir();(folder/'doc.md').write_text('source')
    stored={'previous': 'original evidence'}
    calls=[]
    def encode(texts):
        calls.append(texts)
        if failure=='embedding' and len(calls)==2:
            raise RuntimeError('intentional embedding outage')
        return [[0.0]*1024 for _ in texts]
    class Client:
        def __init__(self, *args, **kwargs): pass
        def upsert(self, collection_name, points, **kwargs):
            for point in points: stored[str(point.id)]='staged'
            if failure=='upsert': raise RuntimeError('intentional partial write')
        def delete(self, collection_name, points_selector, **kwargs):
            for id in points_selector.points: stored.pop(str(id),None)
    monkeypatch.setattr(support.qdrant_client,'QdrantClient',Client)
    monkeypatch.setattr(support,'EMBED_BATCH',1)
    monkeypatch.setattr(support,'qdrant_client_options',lambda *a: {})
    adapter=SimpleNamespace(content_dir=tmp_path,db=StatusTrackingDB(),qdrant_url='http://localhost',collection_name='test',
        embed=SimpleNamespace(encode_sync=encode),
        _sync_markdown_nodes=lambda *args:[{'text':f'Original indexed document passage {i}', 'doc_id':str(i),'payload':{}} for i in range(2)])
    result=QdrantLlamaIndexAdapter._sync_parse(adapter,'ds-1',limit=1)
    assert result['errors']==1
    assert stored=={'previous':'original evidence'}


def test_retire_previous_is_scoped_to_file_dataset_and_keeps_new_ids():
    from backend.index_replacement import retire_previous
    from qdrant_client import QdrantClient, models
    import uuid
    client=QdrantClient(':memory:')
    client.create_collection('test', vectors_config=models.VectorParams(size=2,distance=models.Distance.COSINE))
    ids=[str(uuid.uuid4()) for _ in range(4)]
    client.upsert('test',points=[models.PointStruct(id=id,vector=[1.,0.],payload={'dataset_id':ds,'file_name':name})
        for id,ds,name in zip(ids,['d','d','other','d'],['a','a','a','other'])])
    retire_previous(client,'test','d','a',[ids[1]])
    assert {str(point.id) for point in client.scroll('test',limit=10)[0]}==set(ids[1:])
    client.close()


@pytest.mark.parametrize('current,expected', [('', 'failed question'), ('next question', 'next question')])
def test_failed_chat_restores_draft_without_overwriting_new_input(current, expected):
    from sovushka.pages.chat import _restore_failed_question
    input=SimpleNamespace(value=current)
    input.set_value=lambda value:setattr(input,'value',value)
    saved=[]
    _restore_failed_question(input,SimpleNamespace(save=saved.append),'failed question')
    assert input.value==expected
    assert saved==(['failed question'] if not current else [])
def test_chat_context_preserves_hybrid_order_over_lexical_keyword_density():
    from types import SimpleNamespace
    from proxy.services.saferag_service import rank_chunks_for_question, concentrate_sources
    evidence = SimpleNamespace(content="Два устройства по 1250 кВА", doc_name="answer.pdf", score=0.015)
    noisy = SimpleNamespace(content="Количество мощность трансформаторов: оглавление", doc_name="contents.pdf", score=0.03)
    ordered = rank_chunks_for_question("Количество и мощность трансформаторов", [evidence, noisy], preserve_retrieval_order=True)
    packed = concentrate_sources(ordered, max_docs=2, min_score=float('-inf'), max_chunks=1)
    assert packed == [evidence]
    assert not hasattr(noisy, '_rank_score')
