"""Isolated, opt-in real-model evidence trial; never touches application state.

Runs the production native-hybrid retrieval and parent/evidence reader. It is
not the full chat agent. Human answer review is separate from retrieval recall;
no-answer cases are excluded from recall. No credentials or runtime URLs are
written into the report. Models must be explicitly supplied and already local.
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import math
import os
from pathlib import Path
import sys
import time
import uuid

import httpx
from qdrant_client import AsyncQdrantClient, QdrantClient, models

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from backend.index_replacement import ReplacementJournal
from backend.light_qdrant_runtime import LightQdrantRuntime
from backend.qdrant_retrieval import QdrantRetrieval
from backend.sparse_index import ensure_current
from proxy.services.chat_section_context_service import ChatSectionReader
from proxy.services.evidence_packet_service import build_retrieval_evidence_packet, render_retrieval_evidence_for_model


def validate_fixture(fixture):
    docs, cases = fixture['documents'], fixture['cases']
    ids = {d['id'] for d in docs}
    if not docs or not cases or len(ids) != len(docs):
        raise ValueError('Empty fixture or duplicate document IDs')
    if len({c['id'] for c in cases}) != len(cases):
        raise ValueError('Duplicate case IDs')
    for case in cases:
        if not case['question'].strip() or not set(case['gold_documents']) <= ids:
            raise ValueError('Invalid question or gold document')
    return docs, cases


def retrieval_metrics(documents, gold, k=5):
    """Document coverage and binary nDCG; duplicate chunks earn no extra credit."""
    gold = set(gold)
    if not gold:
        return {'recall5': None, 'ndcg5': None}
    seen, dcg = set(), 0.
    for rank, document in enumerate(documents[:k]):
        if document in gold and document not in seen:
            dcg += 1 / math.log2(rank + 2)
            seen.add(document)
    ideal = sum(1 / math.log2(rank + 2) for rank in range(min(k, len(gold))))
    return {'recall5': len(seen) / len(gold), 'ndcg5': dcg / ideal}


class TrialAdapter(QdrantRetrieval):
    async def _ensure_collection(self):
        pass  # Created explicitly by the trial, without user model roles.

    def _assert_dense_index_contract(self):
        pass  # Same embedding request produces all document/query vectors below.

    async def _prepare_sparse_index(self):
        pass  # Prepared explicitly before any retrieval, under the writer lease.


async def evaluate(args):
    raw = args.fixture.read_bytes()
    docs, cases = validate_fixture(json.loads(raw))
    args.output.mkdir(parents=True, exist_ok=False)
    runtime = LightQdrantRuntime(args.qdrant_exe, args.output / 'runtime')
    report = {'schema': 'les.semantic-trial.v1', 'fixture_sha256': hashlib.sha256(raw).hexdigest(),
        'embedding_model': args.embedding_model, 'chat_model': args.chat_model,
        'scope': 'production native hybrid + parent reader + evidence packet; standalone answer prompt, not full chat agent',
        'documents': len(docs), 'cases': []}
    client = None
    old_schema = os.environ.get('RAG_QDRANT_SCHEMA')
    os.environ['RAG_QDRANT_SCHEMA'] = 'named'
    try:
        async with httpx.AsyncClient(base_url=args.ollama_url, timeout=180, trust_env=False) as http:
            tags = (await http.get('/api/tags')).raise_for_status().json()
            available = {m['name'] for m in tags['models']}
            if not {args.embedding_model, args.chat_model} <= available:
                raise ValueError('Explicit trial models must already be installed')
            response = (await http.post('/api/embed', json={'model': args.embedding_model,
                'input': [d['text'] for d in docs] + [c['question'] for c in cases],
                'truncate': False, 'keep_alive': 0})).raise_for_status().json()
            vectors = response['embeddings']
            dimension = len(vectors[0])
            if len(vectors) != len(docs) + len(cases) or any(len(v) != dimension for v in vectors):
                raise ValueError('Embedding response violates fixture contract')
            report['embedding_dimensions'] = dimension
            runtime.start()
            client = AsyncQdrantClient(url=runtime.url, api_key=runtime.api_key, check_compatibility=False)
            collection = 'les_trial_semantic'
            await client.create_collection(collection, vectors_config={'dense': models.VectorParams(
                size=dimension, distance=models.Distance.COSINE)}, sparse_vectors_config={'bm25_sparse': models.SparseVectorParams()})
            points = [models.PointStruct(id=str(uuid.uuid5(uuid.NAMESPACE_URL, d['id'])),
                vector={'dense': vectors[i]}, payload={'dataset_id': 'synthetic', 'file_name': d['id']+'.md',
                'doc_id': d['id'], 'parent_id': d['id'], 'chunk_ord': 0, 'page': 1,
                'node_role': 'evidence', 'text': d['text']}) for i, d in enumerate(docs)]
            await client.upsert(collection, points=points)
            journal = ReplacementJournal(args.output / 'runtime', collection)
            sync = QdrantClient(url=runtime.url, api_key=runtime.api_key, check_compatibility=False)
            try:
                ensure_current(sync, journal)
            finally:
                sync.close()
            adapter = TrialAdapter()
            adapter.aclient, adapter.collection_name, adapter.content_dir = client, collection, args.output / 'runtime'
            for i, case in enumerate(cases):
                start = time.perf_counter()
                hits = await adapter.retrieve_native_hybrid(case['question'], dataset_ids=['synthetic'], top_k=5,
                    node_roles=['evidence'], _query_state=(vectors[len(docs)+i], journal, journal.read_stamp()))
                expanded = await ChatSectionReader(adapter, ['synthetic']).expand(hits)
                packet = build_retrieval_evidence_packet(question=case['question'], chunks=expanded.chunks, retrieval_trace={})
                evidence = render_retrieval_evidence_for_model(packet, max_chars=16000)
                sources = packet.source_map(max_chars=16000)
                response = (await http.post('/api/chat', json={'model': args.chat_model, 'stream': False,
                    'think': False, 'keep_alive': 0, 'options': {'temperature': 0, 'num_predict': 350, 'num_ctx': 8192},
                    'messages': [{'role': 'system', 'content': 'Ответь кратко по источникам. Учитывай отменённые редакции и исключения. При неразрешимом противоречии покажи обе версии и не выбирай без основания. Если ответа нет, прямо скажи. Каждое фактическое утверждение снабди ссылкой [N] из источников.'},
                    {'role': 'user', 'content': evidence+'\nВопрос: '+case['question']}]})).raise_for_status().json()
                gold = set(case['gold_documents'])
                row = {**case, 'retrieved_documents': [h.doc_id for h in hits],
                    **retrieval_metrics([h.doc_id for h in hits], gold),
                    'answer': response['message']['content'], 'sources': sources,
                    'elapsed_s': round(time.perf_counter()-start, 2)}
                report['cases'].append(row)
                (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n')
                print(f"{case['id']}: recall5={row['recall5']} ({row['elapsed_s']}s)", flush=True)
            scored = [c['recall5'] for c in report['cases'] if c['recall5'] is not None]
            report['summary'] = {'recall5': sum(scored)/len(scored), 'scored_cases': len(scored),
                'ndcg5': sum(c['ndcg5'] for c in report['cases'] if c['ndcg5'] is not None)/len(scored),
                'unanswerable_cases': len(cases)-len(scored), 'answer_review': 'Human review required; recall is not answer correctness'}
            (args.output / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n')
    finally:
        if client:
            await client.close()
        runtime.stop()
        if old_schema is None:
            os.environ.pop('RAG_QDRANT_SCHEMA', None)
        else:
            os.environ['RAG_QDRANT_SCHEMA'] = old_schema
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--qdrant-exe', required=True, type=Path)
    parser.add_argument('--ollama-url', required=True)
    parser.add_argument('--embedding-model', required=True)
    parser.add_argument('--chat-model', required=True)
    args = parser.parse_args()
    asyncio.run(evaluate(args))


if __name__ == '__main__':
    main()
