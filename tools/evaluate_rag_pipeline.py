"""Replay the real retrieval and context pipeline on frozen vectors, without LLMs.

Only a new output directory owns writes and the native Qdrant process. Private
exports stay local; the summary contains metrics, never source text or names.
"""
from __future__ import annotations

import argparse
import asyncio
from contextlib import closing
from dataclasses import asdict
import hashlib
import json
import logging
import math
import os
from pathlib import Path
import statistics
import sys
import time
import psutil

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from qdrant_client import QdrantClient, AsyncQdrantClient, models
from backend.index_replacement import ReplacementJournal
from backend.inference.bm25_sparse import encode_bm25
from backend.light_qdrant_runtime import LightQdrantRuntime
from backend.qdrant_retrieval import QdrantRetrieval
from backend.sparse_index import ensure_current
from proxy.services.chat_section_context_service import ChatSectionReader
from proxy.services.chat_evidence_context import govern_inference_messages, model_visible_source_map, source_context_blocks
from proxy.services.evidence_packet_service import build_retrieval_evidence_packet, render_retrieval_evidence_for_model
from proxy.services.lexical_index_service import LexicalIndex
from proxy.services.model_execution_preset_service import ModelExecutionPreset
from proxy.services.retrieval_service import retrieve_chat_chunks
from proxy.services.retrieval_trial_policy import TRIAL_PROFILES

LOG = logging.getLogger(__name__)


def percentile(values, fraction):
    if not values:
        return None
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    return ordered[lower] + (ordered[math.ceil(position)] - ordered[lower]) * (position - lower)


def evidence_metrics(chunks, case):
    groups = case.get('gold_groups') or []
    if not groups:
        return {'recall5': None, 'recall10': None, 'mrr': None, 'complete_support': None}
    ids = [str(chunk.meta.get('qdrant_point_id') or chunk.meta.get('point_id') or '') for chunk in chunks]
    supports = [{str(item['point_id']) for item in group['support']} for group in groups]
    matches = [next((rank for rank, point in enumerate(ids, 1) if point in group), None) for group in supports]
    return {'recall5': sum(rank is not None and rank <= 5 for rank in matches)/len(groups),
            'recall10': sum(rank is not None and rank <= 10 for rank in matches)/len(groups),
            'mrr': 1/min((rank for rank in matches if rank), default=math.inf),
            'complete_support': all(rank is not None for rank in matches)}


def validate_inputs(snapshot, queries, cases):
    fingerprint = snapshot.get('embedding_fingerprint')
    if not fingerprint or fingerprint != queries.get('embedding_fingerprint'):
        raise ValueError('Snapshot and query embedding contracts differ')
    points = snapshot['points']
    point_ids = {str(point['id']) for point in points}
    if not points or len(point_ids) != len(points):
        raise ValueError('Empty or duplicate snapshot points')
    dimensions = {len(point['vector']['dense']) for point in points}
    if len(dimensions) != 1 or any(point['payload'].get('embedding_fingerprint') != fingerprint for point in points):
        raise ValueError('Snapshot mixes embedding contracts or dimensions')
    dimension = dimensions.pop()
    if dimension <= 0 or any(not math.isfinite(float(value)) for point in points for value in point['vector']['dense']):
        raise ValueError('Empty or nonfinite document vector')
    by_query = {query['id']: query for query in queries['queries']}
    if len(by_query) != len(queries['queries']) or len({case['id'] for case in cases}) != len(cases):
        raise ValueError('Duplicate query or case identity')
    for case in cases:
        query = by_query.get(case['id'])
        if not query or query['question'] != case['question'] or not query.get('dataset_ids') or len(query['dense']) != dimension:
            raise ValueError('Query text, vector or scope differs from control case')
        if not all(math.isfinite(float(value)) for value in query['dense']):
            raise ValueError('Nonfinite query vector')
        for group in case.get('gold_groups') or []:
            support = {str(item['point_id']) for item in group['support']}
            if not support or support - point_ids:
                raise ValueError('Gold support is empty or absent from frozen corpus')
    return dimension, by_query


class FrozenRetrieval(QdrantRetrieval):
    async def _ensure_collection(self): pass
    def _assert_dense_index_contract(self): pass
    async def _prepare_sparse_index(self):
        return ensure_current(self.sync, ReplacementJournal.for_adapter(self))


class FrozenEmbedding:
    def __init__(self, queries):
        self.vectors = {query['question']: query['dense'] for query in queries}
        if len(self.vectors) != len(queries):
            raise ValueError('Duplicate question text in query export')

    async def encode_async(self, texts, **kwargs):
        # Unknown/rephrased queries fail: no fallback model or gold-derived text.
        return [self.vectors[' '.join(text.split())] for text in texts]


def summarize(rows):
    successes = [row for row in rows if not row.get('error')]
    answerable = [row for row in rows if row['answerable']]
    negatives = [row for row in rows if not row['answerable']]
    result = {'cases': len(rows), 'errors': len(rows)-len(successes),
              'answerable': len(answerable), 'unanswerable': len(negatives),
              'unanswerable_good_search': sum(row.get('quality_status') == 'good' for row in negatives),
              'semantic_accuracy': None, 'semantic_accuracy_reason': 'No generator or judge called'}
    for phase in ('search', 'model_packet'):
        result[phase] = {metric: statistics.mean(row.get(phase, {}).get(metric, 0.) for row in answerable) if answerable else None
                         for metric in ('recall5', 'recall10', 'mrr', 'complete_support')}
    for metric in ('search_ms', 'context_ms', 'context_tokens', 'python_rss_bytes'):
        values = [row[metric] for row in successes if metric in row]
        result[metric] = {'p50': percentile(values, .5), 'p95': percentile(values, .95)}
    return result


async def compare(adapter, cases, by_query, out, profile_names, rank_constants, input_tokens):
    preset = ModelExecutionPreset(preset_id='offline-trial', model_family='frozen', input_token_limit=input_tokens,
        generation_reserve_tokens=1200, safety_reserve_tokens=512, normal_tool_count=3, max_tools=5,
        max_batch_items=5, parallel_read_limit=1, reasoning_enabled=False, source_chain=('offline_trial',))
    runs = []
    # Rotate the profile order by case to distribute first-query/cache effects.
    variants = [(name, constant) for name in profile_names for constant in rank_constants]
    for index, case in enumerate(cases):
        order = variants[index % len(variants):] + variants[:index % len(variants)]
        for name, constant in order:
            row = {'case_id': case['id'], 'profile': name, 'rrf_constant': constant,
                   'answerable': bool(case.get('gold_groups'))}
            adapter.rrf_rank_constant = constant
            process = psutil.Process()
            try:
                started = time.perf_counter()
                retrieval = await retrieve_chat_chunks(question=case['question'], dataset_ids=by_query[case['id']]['dataset_ids'],
                    rag_backend=adapter, reranker_enabled=False, reranker_available=False, reranker_cls=None,
                    mlx_url='', logger=LOG, return_trace=True, scope_source='frozen_export',
                    trial_policy=TRIAL_PROFILES[name])
                row['search_ms'] = (time.perf_counter()-started)*1000
                if retrieval.trace.status == 'blocked':
                    raise ValueError(retrieval.trace.error_code)
                row['search'] = evidence_metrics(retrieval.chunks, case)
                row['quality_status'] = retrieval.quality.status
                started = time.perf_counter()
                reader = ChatSectionReader(adapter, by_query[case['id']]['dataset_ids'])
                expanded = await reader.expand(retrieval.chunks)
                evidence = build_retrieval_evidence_packet(question=case['question'], chunks=expanded.chunks,
                    retrieval_trace=retrieval.payload())
                context = render_retrieval_evidence_for_model(evidence, max_chars=input_tokens*4)
                sources = evidence.source_map(max_chars=input_tokens*4)
                messages, packet = govern_inference_messages(preset=preset, profile_prefix='Answer from evidence; cite sources. State missing evidence.',
                    request_payload=case['question'], evidence=source_context_blocks(context), source_map=sources)
                visible = model_visible_source_map(packet, sources)
                reader.assert_for_inference()
                visible_ids = {source.get('qdrant_point_id') for source in visible}
                row['model_packet'] = evidence_metrics([chunk for chunk in expanded.chunks if chunk.meta['qdrant_point_id'] in visible_ids], case)
                row['context_ms'] = (time.perf_counter()-started)*1000
                row['context_tokens'] = packet.included_tokens
                row['python_rss_bytes'] = process.memory_info().rss
                row['section_coverage'] = reader.coverage(visible)
                trace = retrieval.payload()
                row['trace'] = {key: trace.get(key) for key in (
                    'status', 'error_code', 'mode', 'score_kind', 'retrieval_channels', 'fusion',
                    'lexical_count', 'retry_count', 'quality_status', 'quality_detail', 'rerank', 'raptor', 'colbert')}
                # Evidence for local manual review; never copy it into a public report.
                target = out/'evidence'/f"{index:04d}-{name}-{constant:g}.json"
                target.parent.mkdir(exist_ok=True)
                target.write_text(json.dumps({'case_id': case['id'], 'question': case['question'],
                    'messages': messages, 'sources': visible}, ensure_ascii=False, indent=2), encoding='utf-8')
            except Exception as error:
                row['error'] = type(error).__name__
                row['error_code'] = str(getattr(error, 'detail', type(error).__name__))
            runs.append(row)
        print(f'case {index+1}/{len(cases)}', flush=True)
    return runs


def evaluate(snapshot, queries, cases, out, executable, *, profiles, rank_constants=(2.,), input_tokens=8192):
    dimension, by_query = validate_inputs(snapshot, queries, cases)
    if not profiles or set(profiles)-TRIAL_PROFILES.keys() or not rank_constants or any(not 0 < value < math.inf for value in rank_constants):
        raise ValueError('Unknown profiles or invalid RRF constants')
    if input_tokens <= 1712:
        raise ValueError('Input budget must exceed generation and safety reserves')
    out = Path(out).resolve()
    out.mkdir(parents=True, exist_ok=False)
    runtime = LightQdrantRuntime(Path(executable), out/'qdrant')
    environment = {'RAG_QDRANT_SCHEMA': 'named', 'RAG_LEXICAL_DB_PATH': str(out/'lexical.sqlite'),
                   'LES_RAG_ADVANCED_POLICY_PATH': str(out/'advanced-policy.json'), 'LES_RAG_ADVANCED_STATUS_PATH': str(out/'advanced-status.json')}
    previous = {key: os.environ.get(key) for key in environment}
    os.environ.update(environment)
    try:
        runtime.start()
        with closing(QdrantClient(url=runtime.url, api_key=runtime.api_key, check_compatibility=False)) as sync:
            sync.create_collection('trial', vectors_config={'dense': models.VectorParams(size=dimension, distance=models.Distance.COSINE)},
                sparse_vectors_config={'bm25_sparse': models.SparseVectorParams(modifier=models.Modifier.IDF)})
            for start in range(0, len(snapshot['points']), 128):
                points = []
                for point in snapshot['points'][start:start+128]:
                    sparse = encode_bm25(point['payload']['text'])
                    points.append(models.PointStruct(id=point['id'], payload=point['payload'], vector={
                        'dense': point['vector']['dense'], 'bm25_sparse': models.SparseVector(indices=list(sparse), values=list(sparse.values()))}))
                sync.upsert('trial', points=points, wait=True)
            lexical = LexicalIndex(str(out/'lexical.sqlite'))
            lexical.upsert_chunks('trial', [{'point_id': point['id'], **point['payload']} for point in snapshot['points']])
            lexical.mark_collection('trial', point_count=len(snapshot['points']), indexed_count=len(snapshot['points']))
            adapter = FrozenRetrieval()
            adapter.sync, adapter.collection_name, adapter.content_dir = sync, 'trial', out/'content'
            adapter.embed = FrozenEmbedding(queries['queries'])
            adapter._lexical_storage = 'sqlite-postings'
            ensure_current(sync, ReplacementJournal.for_adapter(adapter))
            async def run():
                client = AsyncQdrantClient(url=runtime.url, api_key=runtime.api_key, check_compatibility=False)
                try:
                    adapter.aclient = client
                    return await compare(adapter, cases, by_query, out, profiles, rank_constants, input_tokens)
                finally:
                    await client.close()
            runs = asyncio.run(run())
        summary = {f'{name}/rrf={constant:g}': summarize([row for row in runs if row['profile'] == name and row['rrf_constant'] == constant])
                   for name in profiles for constant in rank_constants}
        report = {'schema': 'les.rag-pipeline-trial.v1', 'scope': 'frozen retrieval and production model-packet packing; no model calls',
                  'input_sha256': {name: hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
                                   for name, value in (('snapshot', snapshot), ('queries', queries), ('cases', cases))},
                  'profiles': [asdict(TRIAL_PROFILES[name]) for name in profiles], 'input_tokens': input_tokens,
                  'summary': summary, 'runs': runs}
        (out/'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
        (out/'summary.json').write_text(json.dumps(summary, indent=2), encoding='utf-8')
        return report
    finally:
        runtime.stop()
        for key, value in previous.items():
            if value is None: os.environ.pop(key, None)
            else: os.environ[key] = value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('snapshot', 'queries', 'cases', 'output', 'qdrant-exe'):
        parser.add_argument('--'+name, required=True, type=Path)
    parser.add_argument('--profiles', default='current,minimal,hierarchy,without_lexical_safety,without_exact_promotions')
    parser.add_argument('--rrf-constants', default='2')
    parser.add_argument('--input-tokens', type=int, default=8192)
    args = parser.parse_args()
    read = lambda path: json.loads(path.read_text(encoding='utf-8'))
    report = evaluate(read(args.snapshot), read(args.queries), read(args.cases), args.output, args.qdrant_exe,
        profiles=args.profiles.split(','), rank_constants=[float(value) for value in args.rrf_constants.split(',')], input_tokens=args.input_tokens)
    print(json.dumps(report['summary'], indent=2))


if __name__ == '__main__':
    main()
