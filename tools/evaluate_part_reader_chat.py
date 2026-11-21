"""Exercise the experimental reader through an isolated native chat on authored TXT folders."""
import argparse
from contextlib import contextmanager
from dataclasses import replace
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
import tempfile
import time
from types import SimpleNamespace
from unittest.mock import patch

import httpx
from llama_index.core import Settings
import psutil

from proxy.services.model_connection_contracts import ConnectionLocality, ConnectionRole
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_intake_execution import settings
from tools.evaluate_llamaindex_folder import render
from tools.prepare_rag_trial_fixture import local_base


def folders(fixtures):
    pump = json.loads((fixtures / 'folder-chat-v1.json').read_bytes())
    questions = {row['id']: row['question'] for row in pump['cases']}
    yield ('pump-without-south', {row['id']: row['text'] for row in pump['documents'] if row['id'] != '056'},
           {key: questions[key] for key in ('pump', 'pump-rephrased', 'warranty')})
    documents = {row['id']: row['text'] for row in json.loads((fixtures / 'full-chat-v1.json').read_bytes())['documents']
                 if row['id'] in {'delivery-old', 'delivery-new', 'refund-old', 'refund-new', 'shipping', 'hours-new'}}
    current = 'Каковы действующие сроки доставки заказа и возврата товара? Укажи срок и единицу для каждого.'
    yield 'editions-full', documents, {'current': current}
    yield ('editions-without-current-refund', {k: v for k, v in documents.items() if k != 'refund-new'},
           {'current': current, 'historical': 'Какой срок возврата допускала редакция 2029 года?'})


def unload(server, models):
    loaded = server.get('/api/ps').raise_for_status().json()['models']
    for entry in loaded:
        if entry['name'] in models:
            server.post('/api/generate', json={'model': entry['name'], 'keep_alive': 0}).raise_for_status()


def post(api, path, data, server, models):
    for attempt in range(3):
        if psutil.virtual_memory().available < 4 * 2**30:
            unload(server, models)
            deadline = time.monotonic() + 120
            while psutil.virtual_memory().available < 4 * 2**30 and time.monotonic() < deadline:
                time.sleep(1)
        reply = api.post(path, json=data, timeout=300)
        if reply.is_success or reply.json().get('code') not in {'CHAT_MEMORY_GUARD', 'MODEL_MEMORY_GUARD'}:
            return reply
        if attempt < 2:
            unload(server, models)
    return reply


def index_folder(t, path, name, documents, server, models):
    path.mkdir()
    for key, text in documents.items():
        (path / (key + '.txt')).write_bytes(text.encode())
    dataset = t.client.post('/api/rag/datasets', json={'name': name}).raise_for_status().json()['id']
    job = t.client.post('/api/rag/index-external', json={
        'path': str(path), 'dataset_id': dataset, 'background': True}).raise_for_status().json()['intake_job_id']
    deadline = time.monotonic() + 420
    while time.monotonic() < deadline:
        status = t.client.get('/api/light/intake', params={'id': job}).raise_for_status().json()['status']
        if status == 'completed':
            break
        if status == 'waiting':
            post(t.client, '/api/light/intake', {'id': job}, server, models).raise_for_status()
        if status in ('failed', 'blocked', 'error'):
            raise RuntimeError('Indexing ' + status)
        time.sleep(1)
    with sqlite3.connect(t.database) as db:
        count = db.execute("SELECT count(*) FROM documents WHERE dataset_id=? AND status='INDEXED'", (dataset,)).fetchone()[0]
    if count != len(documents) or status != 'completed':
        raise ValueError(f'Only {count}/{len(documents)} files indexed')
    return dataset


@contextmanager
def compact_test_index(directory):
    write_text = Path.write_text

    def write_config(path, content, *args, **kwargs):
        if path.name == 'runtime.yaml' and path.is_relative_to(directory):
            config = json.loads(content)
            config['storage']['optimizers'] = {'default_segment_number': 1}
            config['storage']['wal'] = {'wal_capacity_mb': 4, 'wal_segments_ahead': 0}
            content = json.dumps(config)
        return write_text(path, content, *args, **kwargs)

    with patch.object(Path, 'write_text', write_config):
        yield


def chat(t, dataset, case, question, documents, model_client, server, models):
    session = 'parts-' + dataset + '-' + case
    captures = []

    def respond(body):
        with sqlite3.connect(t.database) as db:
            trace, = db.execute('SELECT retrieval_trace_json FROM chat_history WHERE session_id=? ORDER BY id DESC LIMIT 1',
                                (session,)).fetchone()
        sources = sorted(json.loads(trace)['source_map'], key=lambda s: s['index'])
        packed = '\n'.join(str(m.get('content', '')) for m in body['messages'])
        ordered = []
        for number, source in enumerate(sources, 1):
            key = Path(source['doc_name']).stem
            text = documents[key]
            if (source['index'] != number or text not in packed or
                    hashlib.sha256(text.encode()).hexdigest() != source['source_sha256']):
                raise ValueError('Native source identity mismatch')
            ordered.append(key)
        wire = {key: body.get(key) for key in ('model', 'temperature', 'reasoning_effort')}
        wire.update(visible=ordered, max_tokens=body.get('max_tokens', body.get('max_completion_tokens')))
        result = render({'case': case, 'variant': 'http', 'question': question, 'calls': [wire]},
                        documents, model_client, 'parts')
        captures.append(result)
        if not result.get('complete_source_map'):
            raise ValueError('Incomplete synthesis: ' + str(result.get('error')))
        return {'text': result['answer']}

    t.scripts[:] = [respond] * 4
    reply = post(t.client, '/api/chat', {'question': question, 'session_id': session, 'mode': 'search',
                 'retrieval_mode': 'hybrid', 'scope': {'scope_type': 'datasets', 'dataset_ids': [dataset]}}, server, models)
    data = reply.json()
    linked = (data.get('retrieval_trace', {}).get('citation_check') or {}).get('linked', [])
    originals = {}
    for source in data.get('source_map', []):
        if source['index'] in linked:
            name = source['doc_name']
            raw = t.client.get('/api/documents/by-id/' + source['doc_id'] + '/raw').raise_for_status().content
            digest = hashlib.sha256(raw).hexdigest()
            if raw != documents[Path(name).stem].encode() or digest != source['source_sha256']:
                raise ValueError('Wrong original for citation')
            originals[str(source['index'])] = {'file': name, 'sha256': digest}
    return {'case': case, 'question': question, 'http': reply.status_code, 'error': data.get('code'),
            'answer': data.get('answer'), 'linked_originals': originals, 'attempts': captures}


def run(args, folder_cases=folders, query=chat):
    base = local_base(args.ollama)
    models = {args.model, 'bge-m3:latest', 'bge-m3'}
    report = {'model': args.model, 'embedding': 'bge-m3', 'folders': []}
    cfg = {**settings(), 'EMBEDDING_MODEL': 'bge-m3', 'EMBED_BACKEND': 'ollama', 'RAG_VECTOR_SIZE': '1024'}
    with args.output.open('x', encoding='utf8') as output, httpx.Client(base_url=base, trust_env=False, timeout=180) as server, \
            httpx.Client(base_url=base + '/v1', trust_env=False, timeout=180) as llm:
        try:
            for name, documents, questions in folder_cases(args.fixtures):
                if shutil.disk_usage(args.scratch).free < 20 * 2**30:
                    raise ValueError('Insufficient space for the isolated index')
                with tempfile.TemporaryDirectory(prefix='part-reader-', dir=args.scratch, ignore_cleanup_errors=True) as temp, \
                        compact_test_index(Path(temp).resolve()), \
                        patch('tests.test_light_native_intake_execution.settings', return_value=cfg), \
                        patch.object(Settings, '_tokenizer', str.split), \
                        contextmanager(native_turn.__wrapped__)(Path(temp), SimpleNamespace(param='rag-small')) as t:
                    connection = post(t.client, '/api/model-connections', {'display_name': 'Trial BGE', 'base_url': base + '/v1',
                        'model_id': 'bge-m3', 'locality': 'loopback', 'extension_type': 'ollama'}, server, models).raise_for_status().json()
                    post(t.client, '/api/model-connections/' + connection['connection_id'] + '/test',
                         {'revision_id': connection['revision_id'], 'capabilities': ['embeddings']}, server, models).raise_for_status()
                    t.registry.bind_role(ConnectionRole.EMBEDDINGS, connection['revision_id'],
                        expected_binding_revision=t.registry.get_role_binding(ConnectionRole.EMBEDDINGS).binding_revision, actor='admin:test')
                    dataset = index_folder(t, Path(temp) / 'files', name, documents, server, models)
                    revision = t.registry.create_connection(display_name='Trial part reader', base_url=t.revision.base_url,
                        model_id=args.model, locality=ConnectionLocality.LOOPBACK, requested_context_tokens=8192,
                        secret_ref=None, extension_type=None, actor='admin:test')
                    snapshot = t.registry.latest_capability_snapshot(t.revision.revision_id)
                    t.registry.save_capability_snapshot(replace(snapshot, snapshot_id='cap:part-reader', connection_revision_id=revision.revision_id,
                        transport_options={'max_output_field': 'max_tokens', 'reasoning_control': 'reasoning_effort_none'}), actor='admin:test')
                    t.registry.bind_role(ConnectionRole.ANSWER, revision.revision_id,
                        expected_binding_revision=t.registry.get_role_binding(ConnectionRole.ANSWER).binding_revision, actor='admin:test')
                    entry = {'folder': name, 'indexed': len(documents), 'cases': []}
                    report['folders'].append(entry)
                    for case, question in questions.items():
                        row = query(t, dataset, case, question, documents, llm, server, models)
                        entry['cases'].append(row)
                        output.seek(0)
                        json.dump(report, output, ensure_ascii=False, indent=2)
                        output.write('\n')
                        output.truncate()
                        output.flush()
                        print(name, case, row['http'], row['error'], list(row['linked_originals']), flush=True)
        finally:
            unload(server, models)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--fixtures', type=Path, default=Path('tests/fixtures/rag'))
    parser.add_argument('--ollama', required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    run(parser.parse_args())
