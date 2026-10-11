"""Opt-in full-chat acceptance over authored synthetic sources.

Uses the installed application's upload, ingestion, SSE chat and original-source
API, with explicit dataset and instance identity. Does not read chat history,
application prompts, logs or other datasets. Reports coverage, never invents a
semantic pass label. Review generated answers against the fixed rubric.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
from io import BytesIO, StringIO
import json
from pathlib import Path
import time
import uuid

import httpx
from openpyxl import Workbook
from PIL import Image, ImageDraw, ImageFont


def materialize(document, font=None):
    kind = document.get('format', 'md')
    output = BytesIO()
    if kind == 'scan_pdf':
        if font is None:
            raise ValueError('Raster PDF fixtures require an explicit --font')
        image = Image.new('RGB', (1800, 600), 'white')
        try:
            ImageDraw.Draw(image).text((80, 180), document['text'], fill='black',
                font=ImageFont.truetype(str(font), 40))
            image.save(output, format='PDF', resolution=150)
        finally:
            image.close()
        return document['id']+'.pdf', output.getvalue()
    if kind == 'xlsx':
        book = Workbook(); sheet = book.active; sheet.title = 'Данные'
        for row in document['rows']:
            sheet.append(row)
        book.save(output); book.close()
        return document['id']+'.xlsx', output.getvalue()
    if kind == 'csv':
        text = StringIO(); csv.writer(text).writerows(document['rows'])
        return document['id']+'.csv', text.getvalue().encode('utf-8-sig')
    if kind != 'md':
        raise ValueError('Unsupported authored document format')
    return document['id']+'.md', document['text'].encode('utf-8')


def stream_final(response):
    event, data = 'message', []
    progress = []
    for line in response.iter_lines():
        if not line:
            if data:
                payload = json.loads('\n'.join(data))
                if event in {'final', 'error'}:
                    return event, payload, progress
                if event == 'tool_progress':
                    progress.append({key: payload[key] for key in ('tool', 'status') if key in payload})
            event, data = 'message', []
        elif line.startswith('event:'):
            event = line[6:].strip()
        elif line.startswith('data:'):
            data.append(line[5:].strip())
    raise ValueError('Chat stream ended without a final/error frame')


def coverage(sources, gold):
    if not gold:
        return None
    found = {Path(s.get('doc_name', '')).stem for s in sources}
    return len(found & set(gold))/len(set(gold))


def safe_source(source):
    return {key: source[key] for key in ('index','doc_name','quote','snippet','page','source_page',
        'context_origin','section_fragment_count') if key in source}


def unload(http, models):
    for model in models:
        response = http.post('/api/generate', json={'model': model, 'keep_alive': 0})
        response.raise_for_status()
    deadline = time.monotonic() + 30
    while models:
        loaded = {row['model'] for row in http.get('/api/ps').raise_for_status().json()['models']}
        if not loaded.intersection(models):
            break
        if time.monotonic() >= deadline:
            raise ValueError('Trial models did not unload; stop instead of flooding rejected requests')
        time.sleep(.25)


def wait_admission(api, *, timeout=60):
    """Wait for the application's protection after explicit test-model unloading."""
    started = time.monotonic()
    while True:
        state = api.get('/api/indexing-mode').raise_for_status().json()
        if state.get('chat_generation_allowed') is True:
            return round(time.monotonic() - started, 2)
        if time.monotonic() - started >= timeout:
            raise ValueError('Trial admission did not recover; protection remains enabled')
        time.sleep(.5)


def evaluate(args):
    raw = args.fixture.read_bytes(); fixture = json.loads(raw)
    documents, cases = fixture['documents'], fixture['cases']
    identities = {document['id'] for document in documents}
    if len(identities) != len(documents) or len({case['id'] for case in cases}) != len(cases):
        raise ValueError('Duplicate fixture identity')
    if any(set(case['gold_documents']) - identities for case in cases):
        raise ValueError('Gold document is missing from fixture')
    if args.case_ids:
        selected = set(args.case_ids.split(','))
        cases = [case for case in cases if case['id'] in selected]
        if selected != {case['id'] for case in cases}:
            raise ValueError('Unknown case identity')
    if not cases:
        raise ValueError('Empty case selection')
    args.output.mkdir(parents=True, exist_ok=False)
    report = {'schema':'les.full-chat-trial.v1','fixture_sha256':hashlib.sha256(raw).hexdigest(),
        'scope':'installed upload/parse + ordinary SSE chat; isolated synthetic dataset',
        'mode':args.mode,'cases':[], 'ingestion':[], 'originals':[]}
    models = [args.chat_model, args.embedding_model] if args.unload_test_models else []
    with httpx.Client(base_url=args.api_url, timeout=300, trust_env=False) as api, \
         httpx.Client(base_url=args.ollama_url, timeout=30, trust_env=False) as ollama:
        observed = api.get('/api/light/instance').raise_for_status().json()
        if observed.get('instance_id') != args.instance_id:
            raise ValueError('Trial instance identity changed')
        roles = api.get('/api/model-connections/effective').raise_for_status().json()['roles']
        for role, expected in [('answer', args.chat_model), ('embeddings', args.embedding_model)]:
            if (roles.get(role) or {}).get('model_id') != expected:
                raise ValueError('Explicit trial roles do not match the installed instance')
        report['version'] = api.get('/api/version').raise_for_status().json().get('build_number')
        report['models'] = {'chat':args.chat_model, 'embedding':args.embedding_model}
        try:
            if not args.skip_ingest:
                unload(ollama, models)
                for index, document in enumerate(documents):
                    name, data = materialize(document, args.font)
                    response = api.post('/api/rag/upload/'+args.dataset_id, files={'file':(name,data)})
                    response.raise_for_status()
                    deadline = time.monotonic()+180
                    while True:
                        rows = api.get('/api/rag/documents', params={'dataset_id':args.dataset_id,'limit':500}).raise_for_status().json()['documents']
                        status = next((row['status'] for row in rows if row['file_name']==name), 'PENDING')
                        if status in {'INDEXED','ERROR','SKIPPED'} or time.monotonic()>deadline:
                            break
                        time.sleep(.5)
                    report['ingestion'].append({'document':document['id'],'format':document.get('format','md'),
                        'status':status,'sha256':hashlib.sha256(data).hexdigest()})
                    if status == 'INDEXED':
                        row = next(row for row in rows if row['file_name'] == name)
                        original = api.get('/api/documents/by-id/' + row['id'] + '/raw').raise_for_status().content
                        actual_hash = hashlib.sha256(original).hexdigest()
                        expected_hash = hashlib.sha256(data).hexdigest()
                        report['originals'].append({'document':document['id'],
                            'sha256':actual_hash,'byte_exact':actual_hash == expected_hash})
                        if actual_hash != expected_hash:
                            raise ValueError('Original bytes changed during ingestion')
                    print(f"ingest {index+1}/{len(documents)} {name}: {status}", flush=True)
                    (args.output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
                    if status != 'INDEXED':
                        raise ValueError('Trial source failed ingestion; no answer scores produced')
            for index, case in enumerate(cases):
                identity = api.get('/api/light/instance').raise_for_status().json()
                if identity.get('instance_id') != args.instance_id:
                    raise ValueError('Trial instance changed between cases')
                unload(ollama, models)
                admission_wait = wait_admission(api)
                started = time.perf_counter()
                with api.stream('POST','/api/chat/stream',json={'question':case['question'],
                    'dataset_ids':[args.dataset_id], 'session_id':'trial-'+uuid.uuid4().hex,
                    'mode':args.mode, 'response_length':'short', 'semantic_cache_enabled':False,
                    'reranker_enabled':False, 'validation_enabled':False}) as response:
                    response.raise_for_status(); event, final, progress = stream_final(response)
                sources = [safe_source(source) for source in final.get('source_map',[])]
                trace = final.get('retrieval_trace') or {}
                model = final.get('model_connection') or {}
                report['cases'].append({**case,'event':event,'error_code':final.get('code') if event=='error' else None,
                    'answer':final.get('answer',''),'sources':sources,'visible_gold_coverage':coverage(sources,case['gold_documents']),
                    'admission_wait_s':admission_wait,
                    'elapsed_s':round(time.perf_counter()-started,2),'observed_model':model.get('model_id'),
                    'retrieval':{key:trace[key] for key in ('status','mode','quality_status','quality_detail','score_kind','retry_count') if key in trace},
                    'tool_progress':progress})
                (args.output/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
                print(f"case {index+1}/{len(cases)} {case['id']}: {event}, {report['cases'][-1]['elapsed_s']}s", flush=True)
        finally:
            unload(ollama, models)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('api-url','dataset-id','instance-id','ollama-url','chat-model','embedding-model'):
        parser.add_argument('--'+name,required=True)
    parser.add_argument('--fixture',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--font',type=Path)
    parser.add_argument('--mode',choices=['agent','search'],default='agent')
    parser.add_argument('--case-ids')
    parser.add_argument('--skip-ingest',action='store_true')
    parser.add_argument('--unload-test-models',action='store_true',help='Explicitly unload only the supplied trial models between cases')
    evaluate(parser.parse_args())


if __name__ == '__main__':
    main()
