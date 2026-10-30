"""Embed explicitly selected authored text/row fixtures with an existing local model.

Produces frozen inputs for evaluate_rag_pipeline. This normalizes fixture rows;
it does not exercise Office/PDF/OCR ingestion, change role bindings, or pull models.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from urllib.parse import urlparse
import uuid

import httpx


def local_base(url):
    parsed = urlparse(url)
    if parsed.scheme != 'http' or parsed.hostname not in {'127.0.0.1', 'localhost', '::1'} or parsed.username or parsed.password:
        raise ValueError('Trial model server must be an explicit local HTTP endpoint')
    return url.rstrip('/')


def prepare(fixture, out, *, api_url, model, query_prefix='', document_prefix=''):
    documents, cases = fixture['documents'], fixture['cases']
    ids = {doc['id'] for doc in documents}
    if not documents or not cases or len(ids) != len(documents) or len({case['id'] for case in cases}) != len(cases):
        raise ValueError('Empty or duplicate fixture identities')
    if any(set(case['gold_documents']) - ids for case in cases):
        raise ValueError('Gold document absent from authored fixture')
    out = Path(out)
    out.mkdir(parents=True, exist_ok=False)
    texts = []
    for doc in fixture['documents']:
        text = doc.get('text') or '\n'.join(' | '.join(str(cell) for cell in row) for row in doc.get('rows', []))
        if not text.strip():
            raise ValueError('Empty authored document')
        texts.append(text)
    query_texts = [case['question'] for case in fixture['cases']]
    with httpx.Client(base_url=local_base(api_url), timeout=180, trust_env=False) as http:
        known = {row['name']: row for row in http.get('/api/tags').raise_for_status().json()['models']}
        selected = next((row for name, row in known.items() if name == model or name == model+':latest'), None)
        if selected is None:
            raise ValueError('Selected model is not installed; trial never downloads models')
        fingerprint = hashlib.sha256(json.dumps({'model': selected['name'], 'digest': selected['digest'],
            'protocol': 'ollama/api/embed', 'num_ctx': 8192, 'truncate': False,
            'query_prefix': query_prefix, 'document_prefix': document_prefix}, sort_keys=True).encode()).hexdigest()
        vectors = []
        combined = [document_prefix+text for text in texts] + [query_prefix+text for text in query_texts]
        for start in range(0, len(combined), 16):
            values = combined[start:start+16]
            response = http.post('/api/embed', json={'model': selected['name'], 'input': values, 'truncate': False,
                'options': {'num_ctx': 8192}, 'keep_alive': 0 if start+16 >= len(combined) else '60s'}).raise_for_status().json()
            batch = response['embeddings']
            if len(batch) != len(values) or not batch:
                raise ValueError('Embedding response cardinality mismatch')
            vectors.extend(batch)
            print(f'embedded {min(start+16, len(combined))}/{len(combined)}', flush=True)
    dataset = 'authored-fixture'
    points, ids = [], {}
    for doc, text, vector in zip(fixture['documents'], texts, vectors):
        point_id = str(uuid.uuid5(uuid.NAMESPACE_URL, 'les-trial/'+doc['id']))
        ids[doc['id']] = point_id
        points.append({'id': point_id, 'vector': {'dense': vector}, 'payload': {
            'text': text, 'dataset_id': dataset, 'file_name': doc['id']+'.md', 'doc_id': doc['id'],
            'parent_id': 'parent:'+doc['id'], 'chunk_ord': 0, 'page': 1, 'source_ref': doc['id']+'#authored',
            'node_role': 'evidence', 'embedding_fingerprint': fingerprint}})
    cases = [{'id': case['id'], 'question': case['question'], 'category': case['category'],
        'gold_groups': [{'support': [{'point_id': ids[doc]}]} for doc in case['gold_documents']]}
        for case in fixture['cases']]
    queries = [{'id': case['id'], 'question': case['question'], 'dense': vector, 'dataset_ids': [dataset]}
               for case, vector in zip(fixture['cases'], vectors[len(texts):])]
    for name, value in (('snapshot', {'embedding_fingerprint': fingerprint, 'points': points}),
                        ('queries', {'embedding_fingerprint': fingerprint, 'queries': queries}), ('cases', cases)):
        (out/(name+'.json')).write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf-8')
    return {'documents': len(points), 'cases': len(cases), 'embedding_fingerprint': fingerprint,
            'scope': 'authored normalized text/rows, no OCR or ingestion evaluation'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixture', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--api-url', required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--query-prefix', default='')
    parser.add_argument('--document-prefix', default='')
    args = parser.parse_args()
    print(json.dumps(prepare(json.loads(args.fixture.read_text(encoding='utf-8')), args.output,
        api_url=args.api_url, model=args.model, query_prefix=args.query_prefix, document_prefix=args.document_prefix), indent=2))


if __name__ == '__main__': main()
