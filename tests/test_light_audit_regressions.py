"""Generation integrity, concurrent reads and macOS service ownership."""
import asyncio
import os
from pathlib import Path
import subprocess
import socket
import sys
import time
from types import SimpleNamespace

import psutil
import pytest
from fastapi import HTTPException

from backend import qdrant_support as support
from backend.index_replacement import ReplacementJournal
from backend.interface import Chunk, EmbeddingContractError
from backend.qdrant_ingestion import QdrantIngestion
from backend.qdrant_retrieval import QdrantRetrieval


@pytest.mark.skipif(os.name == 'nt', reason='POSIX TCP reuse semantics')
def test_time_wait_is_free_but_live_listener_is_not():
    from backend.light_qdrant_runtime import port_is_free
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(('127.0.0.1', 0)); listener.listen()
    port = listener.getsockname()[1]
    assert not port_is_free(port)
    client = socket.create_connection(('127.0.0.1', port))
    connection, _ = listener.accept()
    connection.close(); client.close(); listener.close()
    assert port_is_free(port)


@pytest.mark.parametrize('change', [False, True])
def test_source_generation_and_conversion_do_not_lock_other_readers(tmp_path, monkeypatch, change):
    source = tmp_path / 'ds/doc.md'
    source.parent.mkdir()
    original = 'Approved delivery January 2031.'
    source.write_text(original)
    writes, statuses, fingerprints = [], [], []
    class DB:
        def get_pending_files(self, *args, **kwargs): return [] if statuses else ['doc.md']
        def update_document_status(self, *args, **kwargs): statuses.append(args)
        def update_dataset_chunk_count(self, *args): pass
        def clear_structured_rules(self, *args): pass
        def set_document_source_fingerprint(self, *args, **kwargs): fingerprints.append(kwargs)
    class Client:
        def __init__(self, *args, **kwargs): pass
        def delete(self, **kwargs): writes.append('delete')
        def upsert(self, **kwargs): writes.append('upsert')
    def encode(texts):
        journal = ReplacementJournal(tmp_path, 'audit')
        with journal.lease():
            journal.assert_clean()  # Other readers can prepare their current index.
        if change: source.write_text('Approved delivery December 2042.')
        return [[.1] * 1024 for _ in texts]
    adapter = SimpleNamespace(content_dir=tmp_path, db=DB(), collection_name='audit',
        qdrant_url='http://127.0.0.1:1', embed=SimpleNamespace(encode_sync=encode),
        _sync_count_file_points=lambda *args: 1)
    monkeypatch.setattr(support, 'qdrant_client_options', lambda *_: {})
    monkeypatch.setattr(support.qdrant_client, 'QdrantClient', Client)
    monkeypatch.setattr(QdrantIngestion, '_convert_file', lambda *_: ('TEXT', [
        {'text': original, 'doc_id': 'document', 'payload': {}}]))
    monkeypatch.setenv('RAG_QDRANT_SCHEMA', 'named')
    monkeypatch.setenv('RAG_CHUNK_UNIT', 'chars')
    result = QdrantIngestion._sync_parse(adapter, 'ds', limit=1)
    if change:
        assert result['errors'] == 1
        assert statuses[-1][2] == 'ERROR'
        assert not writes and not fingerprints  # Previous generation stays untouched.
    else:
        assert result['errors'] == 0 and statuses[-1][2] == 'INDEXED'
        assert fingerprints[0]['file_hash'] == support._sha256_file(source)


@pytest.mark.asyncio
async def test_writer_failure_does_not_leave_dataset_parsing():
    statuses = []
    class Adapter(QdrantIngestion):
        async def _ensure_collection(self): pass
        def _assert_dense_index_contract(self): pass
        def _sync_parse(self, *args): raise EmbeddingContractError('INDEX_UPDATE_BUSY')
    adapter = Adapter()
    adapter.db = SimpleNamespace(update_dataset_status=lambda *args: statuses.append(args[1]))
    with pytest.raises(EmbeddingContractError): await adapter.parse_dataset('ds', limit=1)
    assert statuses == ['PARSING', 'ERROR']


@pytest.mark.asyncio
@pytest.mark.parametrize('text', ['Почему?', 'Где и когда?', '如何安装'])
async def test_empty_lexical_query_keeps_dense_channel(tmp_path, monkeypatch, text):
    requests = []
    async def query(**kwargs):
        requests.append(kwargs)
        return SimpleNamespace(points=[SimpleNamespace(id='point', score=.9,
            payload={'text': 'Evidence', 'doc_id': 'd', 'file_name': 'doc.md'})])
    class Adapter(QdrantRetrieval):
        async def _ensure_collection(self): pass
        def _assert_dense_index_contract(self): pass
        async def _prepare_sparse_index(self): pass
    adapter = Adapter(); adapter.collection_name = 'audit'
    adapter.aclient = SimpleNamespace(query_points=query)
    monkeypatch.setattr(support, '_qdrant_schema_mode', lambda: 'named')
    monkeypatch.setattr('backend.sparse_index.read_contract', lambda *_: {'storage': 'sqlite-postings'})
    chunks = await adapter.retrieve_native_hybrid(text,
        _query_state=([1., 0.], ReplacementJournal(tmp_path, 'audit'), None))
    assert requests[0]['query'] == [1., 0.]
    assert 'prefetch' not in requests[0]
    assert chunks[0].meta['_retrieval_channels'] == ['dense']


@pytest.mark.asyncio
async def test_api_context_rejects_replaced_point_at_same_ordinal(tmp_path):
    from tests.test_light_chat_sections import backend, point, chunk
    from proxy.routers.dataset_search import _read_context
    p = point('January 2031.'); seed = chunk(p)
    b = await backend(tmp_path, [p])
    try:
        await b.aclient.set_payload('docs', payload={'text': 'December 2042.'}, points=[p.id])
        with pytest.raises(HTTPException) as error:
            await _read_context(b, [seed], ['ds'], 8)
        assert error.value.status_code == 409
    finally:
        await b.aclient.close()


@pytest.mark.asyncio
async def test_api_context_budget_keeps_complete_fragments_and_provenance(tmp_path):
    from tests.test_light_chat_sections import backend, point, chunk
    from proxy.routers.dataset_search import _read_context
    seed = point('January 2031.')
    huge = point('Exception ' * 1000, 1)
    b = await backend(tmp_path, [seed, huge])
    try:
        expanded, _ = await _read_context(b, [chunk(seed)], ['ds'], 8, max_chars=200)
        assert len(expanded[0].content) <= 200
        fragments = expanded[0].meta['context_fragments']
        assert len(fragments) == 1 and fragments[0]['content'] == seed.payload['text']
        assert fragments[0]['metadata']['qdrant_point_id'] == seed.id
        assert expanded[0].meta['context_omitted_fragments'] == 1
    finally:
        await b.aclient.close()


def test_irrelevant_single_result_is_not_good():
    from proxy.services.retrieval_quality_service import evaluate_retrieval_quality
    from proxy.services.lexical_index_service import RetrievalTrace
    from proxy.services.kot_service import analyze_question
    q = 'Когда начнется извержение вулкана?'
    quality = evaluate_retrieval_quality(question=q,
        chunks=[Chunk('Invoice total 100 dollars.', 'd', 'invoice.md', .5, {})],
        trace=RetrievalTrace(mode='dense_bm25_hierarchical', score_kind='local_rrf'), kot=analyze_question(q))
    assert quality.status == 'weak' and quality.term_coverage == 0


@pytest.mark.parametrize('host,origin,status', [
    ('127.0.0.1:1234', 'http://127.0.0.1:4321', 200),
    ('localhost:1234', None, 200),
    ('audit.invalid:1234', 'http://audit.invalid:1234', 403),
    ('127.0.0.1:1234', 'https://audit.invalid', 403),
    ('127.0.0.1:1234', 'null', 403),
])
def test_browser_boundary_rejects_foreign_host_and_origin(host, origin, status):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient
    from proxy.light_boundary import LightBrowserBoundary
    app = FastAPI(); app.add_middleware(LightBrowserBoundary)
    @app.post('/action')
    def action(): return {'ok': True}
    headers = {'Host': host}
    if origin: headers['Origin'] = origin
    assert TestClient(app).post('/action', headers=headers).status_code == status


@pytest.mark.skipif(sys.platform != 'darwin', reason='macOS lifetime supervisor')
def test_launcher_crash_terminates_guarded_service():
    script = "from backend.light_processes import owned_command; import subprocess,sys,time; child=subprocess.Popen(owned_command([sys.executable,'-c','import os,time;print(os.getpid(),flush=True);time.sleep(30)']));print(child.pid,flush=True);time.sleep(30)"
    parent = subprocess.Popen([sys.executable, '-c', script], cwd=Path(__file__).resolve().parents[1], stdout=subprocess.PIPE, text=True)
    owned = []
    try:
        owned = [psutil.Process(int(parent.stdout.readline())) for _ in range(2)]
        parent.kill(); parent.wait(timeout=3)
        deadline = time.monotonic() + 6
        def alive(p): return p.is_running() and p.status() != psutil.STATUS_ZOMBIE
        while any(alive(p) for p in owned) and time.monotonic() < deadline: time.sleep(.05)
        assert not any(alive(p) for p in owned)
    finally:
        if parent.poll() is None: parent.kill(); parent.wait()
        for p in owned:
            try: p.kill()
            except psutil.NoSuchProcess: pass
