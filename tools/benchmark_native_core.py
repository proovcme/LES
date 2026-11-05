"""Compare frozen Python/native BM25 storage and preparation on identical authored data, without models."""
from __future__ import annotations

import argparse
import hashlib
import functools
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import time
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))


def worker(output, count, mode, library):
    from backend import bm25_store
    from backend.index_replacement import ReplacementJournal
    import psutil
    os.environ['LES_CORE_MODE'] = 'python' if mode == 'python-cached' else mode
    os.environ['LES_CORE_LIBRARY'] = str(library)
    if mode == 'python-cached':
        from backend.inference import lexical_tokens
        lexical_tokens.stem_russian_word = functools.lru_cache(maxsize=32768)(lexical_tokens.stem_russian_word)
    if mode in {'python','python-cached'}:
        from tests.oracles import bm25_store_779 as bm25_store
        from collections import Counter
        from backend.inference.lexical_tokens import tokenize_current
        # Original pre-770 implementation, with an explicit separate cache control.
        bm25_store.prepare = lambda texts: [Counter(tokenize_current(text)) for text in texts]
    output.mkdir(parents=True, exist_ok=False)
    journal = ReplacementJournal(output/'content', 'benchmark')
    journal.directory.mkdir(parents=True)
    def points(start, stop):
        return [SimpleNamespace(id=i, payload={
            'text': (f'Документ {i}. Линия BB_{i%997} подключена к PE. Требуется 12 м кабеля. '
                     + 'Описание оборудования и порядок проверки. '*(8+i%16)),
            'dataset_id':'synthetic', 'file_name':f'file-{i//100}.txt', 'node_role':'evidence'})
                for i in range(start, stop)]
    started = time.perf_counter()
    if mode == 'native':
        with bm25_store.writer(journal) as db:
            for offset in range(0,count,256):
                bm25_store.put(db, points(offset,min(count,offset+256)))
            bm25_store.commit(db,'benchmark',count)
    else:
        with bm25_store.connect(journal, create=True) as db, db:
            db.execute('BEGIN IMMEDIATE')
            for offset in range(0,count,256):
                bm25_store.put(db, points(offset,min(count,offset+256)))
            db.execute("UPDATE corpus SET revision='benchmark'")
    journal._save({'phase':'idle', 'revision':'benchmark'})
    build_seconds = time.perf_counter()-started
    import sqlite3
    with sqlite3.connect(journal.directory/'bm25.sqlite') as db:
        digest = hashlib.sha256()
        for table, order in [('docs','id'),('postings','term,doc'),('frequencies','term'),('ancestors','ancestor,doc')]:
            for row in db.execute(f'SELECT * FROM {table} ORDER BY {order}'):
                digest.update(json.dumps(row,ensure_ascii=False,separators=(',',':')).encode('utf-8'))
    latencies, queries = [], []
    for i in range(60):
        started = time.perf_counter()
        query = f'BB_{i%30}' if i%2 else 'оборудование порядок проверки кабель'
        hits = bm25_store.search(journal, query, limit=10, node_roles=['evidence'])
        latencies.append((time.perf_counter()-started)*1000)
        assert hits
        queries.append([(pid,round(score,12)) for pid,score in hits])
    memory = psutil.Process().memory_info()
    result = {'mode':mode, 'points':count, 'build_seconds':build_seconds,
              'query_median_ms':statistics.median(latencies), 'query_p95_ms':sorted(latencies)[56],
              'rss_after_bytes':memory.rss, 'peak_working_set_bytes':getattr(memory,'peak_wset',None),
              'postings_sha256':digest.hexdigest(),
              'queries_sha256':hashlib.sha256(json.dumps(queries).encode()).hexdigest()}
    (output/'result.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
    print(json.dumps(result),flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--library', required=True, type=Path)
    parser.add_argument('--sizes', nargs='+', type=int, default=[20000,100000])
    parser.add_argument('--repeats', type=int, default=2)
    parser.add_argument('--worker', choices=['python','python-cached','native'])
    args = parser.parse_args()
    if args.worker:
        worker(args.output, args.sizes[0], args.worker, args.library.resolve())
        return
    if not args.library.is_file() or any(n<1 for n in args.sizes) or args.repeats<1:
        parser.error('An existing library, positive corpus sizes and repeat count are required')
    args.output.mkdir(parents=True, exist_ok=False)
    results = []
    for size in args.sizes:
        for repeat in range(args.repeats):
            # Alternate order to reduce systematic warm-cache bias.
            modes = ['python','python-cached','native']
            for mode in (modes if repeat%2==0 else list(reversed(modes))):
                out = args.output/f'{size}-{repeat}-{mode}'
                subprocess.run([sys.executable,str(Path(__file__).resolve()),'--worker',mode,
                    '--output',str(out),'--library',str(args.library.resolve()),'--sizes',str(size)],check=True)
                results.append(json.loads((out/'result.json').read_text(encoding='utf-8')))
                (args.output/'results.json').write_text(json.dumps(results,indent=2),encoding='utf-8')
        rows = [r for r in results if r['points']==size]
        assert len({r['postings_sha256'] for r in rows})==1, 'Native postings differ'
        assert len({r['queries_sha256'] for r in rows})==1, 'Native rankings/scores differ'
    summaries = []
    for size in args.sizes:
        medians = {mode:statistics.median(r['build_seconds'] for r in results if r['points']==size and r['mode']==mode)
                   for mode in ('python','python-cached','native')}
        summaries.append({'points':size,'build_seconds':medians,'speedup':medians['python']/medians['native'],
                          'speedup_over_cached':medians['python-cached']/medians['native'],
                          'postings_and_queries_equal':True})
    (args.output/'summary.json').write_text(json.dumps(summaries,indent=2),encoding='utf-8')
    print(json.dumps(summaries),flush=True)


if __name__=='__main__': main()
