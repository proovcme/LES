"""Synthetic native-Qdrant load probe; no model calls or user collections.

This measures lexical maintenance cost, not semantic retrieval quality. Dense
vectors are two-dimensional placeholders. Output must be a new directory.
"""
import argparse
from contextlib import closing
import json
from pathlib import Path
import statistics
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qdrant_client import QdrantClient, models
from backend.light_qdrant_runtime import LightQdrantRuntime
from backend.index_replacement import ReplacementJournal
from backend.inference.bm25_sparse import encode_bm25
from backend.sparse_index import ensure_current, search, external_mutation


def make_point(index):
    text = (f"Документ {index}. Линия BB_{index % 997} подключена к PE. "
            + "Описание оборудования и порядок проверки. " * (8 + index % 16))
    terms = encode_bm25(text)
    return models.PointStruct(id=index, payload={'text':text, 'dataset_id':'synthetic',
        'file_name':f'example-{index // 100}.txt', 'source_page':index % 100 + 1},
        vector={'dense':[1., 0.], 'bm25_sparse':models.SparseVector(
            indices=list(terms), values=list(terms.values()))})


def measure(client, journal, count):
    start = time.perf_counter()
    for offset in range(0, count, 256):
        client.upsert('benchmark', points=[make_point(i) for i in range(offset, min(offset+256, count))], wait=True)
    ingestion = time.perf_counter() - start
    start = time.perf_counter()
    contract = ensure_current(client, journal)
    migration = time.perf_counter() - start
    latencies = []
    for index in range(30):
        start = time.perf_counter()
        assert ensure_current(client, journal)['revision'] == contract['revision']
        hits = search(client, journal, f'BB_{index}', limit=10)
        records = client.retrieve('benchmark', ids=[pid for pid, _ in hits], with_payload=True)
        assert records and all(f'BB_{index} ' in p.payload['text'] for p in records)
        latencies.append((time.perf_counter()-start)*1000)
    with external_mutation(journal, ids=[count]):
        client.upsert('benchmark', points=[make_point(count)], wait=True)
    start = time.perf_counter()
    changed = ensure_current(client, journal)
    refresh = time.perf_counter() - start
    assert changed['points'] == count + 1 and changed['revision'] != contract['revision']
    for point in client.retrieve('benchmark', ids=[0,count-1,count], with_vectors=True):
        expected = make_point(point.id)
        assert point.payload == expected.payload and point.vector['dense'] == [1.,0.]
    return dict(points=count, average_terms=contract['profile']['average_length'],
        seed_seconds=ingestion, migrate_seconds=migration, after_one_insert_seconds=refresh,
        unchanged_query_median_ms=statistics.median(latencies),
        unchanged_query_p95_ms=sorted(latencies)[28], queries=len(latencies),
        tokenizer=contract['tokenizer'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--qdrant', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--sizes', nargs='+', type=int, default=[1000, 10000])
    args = parser.parse_args()
    if not args.qdrant.is_file() or any(n < 1 for n in args.sizes):
        parser.error('Existing Qdrant executable and positive corpus sizes are required')
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for index, count in enumerate(args.sizes):
        state = args.output / f'case-{index}-{count}'
        state.mkdir()
        runtime = LightQdrantRuntime(args.qdrant.resolve(), state)
        try:
            runtime.start()
            with closing(QdrantClient(url=runtime.url, api_key=runtime.api_key,
                                     check_compatibility=False, timeout=120)) as client:
                client.create_collection('benchmark', vectors_config={'dense':models.VectorParams(
                    size=2, distance=models.Distance.COSINE)}, sparse_vectors_config={
                    'bm25_sparse':models.SparseVectorParams(modifier=models.Modifier.IDF)})
                result = measure(client, ReplacementJournal(state/'content', 'benchmark'), count)
                results.append(result)
                print(json.dumps(result), flush=True)
                (args.output/'results.json').write_text(json.dumps(results, indent=2), encoding='utf-8')
        finally:
            runtime.stop()


if __name__ == '__main__':
    main()
