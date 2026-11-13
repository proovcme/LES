"""Review ownership and model delivery through the full native HTTP chat turn."""
import hashlib
import json
import sqlite3
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Event, Thread

import pytest
from proxy.services.document_task_store import digest, native_request
from tests.test_light_native_chat_turn import native_turn, source_pdf, PATH, HEADERS
from tests.test_light_native_intake_execution import signed_contract, settings


@pytest.fixture
def review_turn(native_turn):
    t = native_turn
    path, checksum = source_pdf(t)
    # Reverse key order deliberately: Edge unifies serde_json/preserve_order.
    selector = {'file_name': 'folder/Source.pdf', 'dataset_id': 'ds'}
    point = {'id': '00000000-0000-0000-0000-000000000001', 'payload': {
        **selector, 'doc_id': 'registered-document', 'page': 2, 'chunk_ord': 0,
        'text': 'Прочитанный раздел 日本語 😀: требуется 12 м.', 'source_sha256': checksum}}
    queries = []; unavailable = Event()
    class Index(BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def do_POST(self):
            query = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            queries.append(query)
            assert self.headers['api-key'] == 'review-fixture'
            if unavailable.is_set():
                self.send_response(503); self.send_header('Content-Length','0'); self.end_headers(); return
            value = {'points': [point], 'next_page_offset': None} if self.path.endswith('/scroll') else [point]
            body = json.dumps({'status': 'ok', 'result': value}, ensure_ascii=False).encode()
            self.send_response(200); self.send_header('Content-Length', str(len(body)))
            self.end_headers(); self.wfile.write(body)
    server = ThreadingHTTPServer(('127.0.0.1', 0), Index)
    thread = Thread(target=server.serve_forever, daemon=True); thread.start()
    try:
        with sqlite3.connect(t.database) as db:
            db.execute("ALTER TABLE documents ADD COLUMN status TEXT DEFAULT 'INDEXED'")
        contract = t.state/'index-contract.json'
        contract.write_text(json.dumps(signed_contract()), encoding='utf-8')
        config = {**settings(), 'RAG_INDEX_CONTRACT_PATH': str(contract)}
        with (t.state/'.env').open('a', encoding='utf-8') as stream:
            stream.write('\n'.join(f'{k}="{v}"' for k,v in config.items())+'\n')
        t.env.update(LES_LIGHT_QDRANT_URL=f'http://127.0.0.1:{server.server_port}',
                     LES_LIGHT_QDRANT_API_KEY='review-fixture', LES_INTERNAL_INGEST_TOKEN='5'*64)
        t.children[-1].kill(); t.children[-1].wait(timeout=5); t.start()
        sid = t.client.post('/api/workspace/sessions', json={'title': 'Проверка'}).raise_for_status().json()['session_id']
        request = {'question': 'Проверь раздел', 'dataset_ids': ['ds'], 'session_id': sid}
        t.client.post(PATH, json={**request, 'question': 'Prepare'}, headers=HEADERS).raise_for_status()
        access = next(s['function']['name'] for s in t.seen[-1]['tools'] if s['function']['name'].startswith('use_extensions_'))
        def call(operation, **args):
            return {'tool': access, 'args': {'operation': 'call', 'name': 'document_review',
                                           'arguments': {'operation': operation, **args}}}
        snapshot = native_request('review_snapshot', dict(selector=selector, allowed=['ds'], files=[],
            source={'sha256': checksum, 'document_id': 'registered-document'}, points=[point]))
        t.review = dict(path=path, checksum=checksum, selector=selector, point=point, queries=queries, unavailable=unavailable,
            request=request, call=call, unit=digest(selector), fingerprint=snapshot['fingerprint'])
        yield t
    finally:
        server.shutdown(); server.server_close(); thread.join(timeout=5)


def decisions(t, **changes):
    return dict(unit_id=t.review['unit'], request_id='save-one', expected_revision=0,
        source_fingerprint=t.review['fingerprint'], status='reviewed', note='Указано 12 м',
        evidence=[{'fragment_id': t.review['point']['id'], 'quote': '12 м'}], **changes)


def test_review_credits_only_delivered_text_persists_quotes_and_rechecks_changed_original(review_turn):
    t=review_turn; r=t.review; call=r['call']
    t.scripts.extend([call('start', selectors=[r['selector']]), call('record', **decisions(t)),
        call('read'), call('record', **decisions(t)), {'text': 'Записано [Источник 1]'}])
    answer=t.client.post(PATH, json=r['request'], headers=HEADERS).raise_for_status().json()
    rows=[v['result'] for v in answer['artifact']['tool_results']]
    assert rows[1]['status']=='error' and rows[3]['status']=='ok'
    receipt=answer['artifact']['execution_receipt']
    assert receipt['status']=='recorded' and receipt['counts']['reviewed']==1 and receipt['accepted'] is False
    assert answer['source_map'][0]['page']==2 and '#page=2' in answer['source_map'][0]['url']
    assert r['point']['payload']['text'] in json.dumps(t.seen[-2], ensure_ascii=False)
    assert all(q.get('with_vector') is False for q in r['queries'])
    dbpath=t.state/'data/document-tasks.sqlite3'
    with sqlite3.connect(dbpath) as db:
        task,quote=db.execute('SELECT task_id,observation FROM review_events').fetchone()
        assert json.loads(quote)['evidence'][0]['quote']=='12 м'
        assert db.execute('SELECT count(*) FROM review_reads').fetchone()[0]==1
    t.children[-1].kill(); t.children[-1].wait(timeout=5); t.start()
    t.scripts.extend([call('status', task_id=task), {'text':'Проверено в прошлой версии.'}])
    stat=r['path'].stat(); raw=r['path'].read_bytes(); r['path'].write_bytes(b'X'+raw[1:])
    import os
    os.utime(r['path'], ns=(stat.st_atime_ns, stat.st_mtime_ns))
    response=t.client.post(PATH, json={**r['request'], 'question':'Продолжи task_id '+task}, headers=HEADERS).raise_for_status().json()
    assert response['artifact']['execution_receipt']['status']=='needs_recheck'
    assert response['artifact']['execution_receipt']['source_check']=='changed'
    assert response['artifact']['execution_receipt']['source_errors']=={r['unit']:'REVIEW_ORIGINAL_CHANGED'}
    assert response['artifact']['completion_status']=='incomplete'
    assert 'Требуется перепроверка' in response['answer']
    with sqlite3.connect(dbpath) as db:
        assert db.execute('SELECT count(*) FROM review_events').fetchone()[0]==1
    r['path'].write_bytes(raw)


def test_broken_model_does_not_credit_pending_review_text(review_turn):
    t=review_turn; r=t.review; call=r['call']
    t.scripts.extend([call('start', selectors=[r['selector']]), call('read'), {'text':'Часть', 'broken':True}])
    failed=t.client.post(PATH,json=r['request'],headers=HEADERS)
    assert failed.status_code==503 and failed.json()['code']=='UPSTREAM_STREAM_INTERRUPTED'
    with sqlite3.connect(t.state/'data/document-tasks.sqlite3') as db:
        assert db.execute('SELECT count(*) FROM review_reads').fetchone()[0]==0
        assert db.execute('SELECT count(*) FROM review_events').fetchone()[0]==0
    with sqlite3.connect(t.database) as db:
        artifact=json.loads(db.execute('SELECT artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()[0])
    assert artifact['error']['code']=='UPSTREAM_STREAM_INTERRUPTED'
    assert artifact['tool_results'][-1]['result']['result']['text']==r['point']['payload']['text']


def test_long_unicode_review_follows_every_cursor_and_never_replays_old_text(review_turn):
    t=review_turn; r=t.review; call=r['call']
    r['point']['payload']['text']='А😀Б'*1501
    snap=native_request('review_snapshot', dict(selector=r['selector'], allowed=['ds'], files=[],
        source={'sha256':r['checksum'],'document_id':'registered-document'}, points=[r['point']]))
    r['fingerprint']=snap['fingerprint']
    arguments=decisions(t); arguments['evidence'][0]['quote']='А😀Б'
    t.scripts.extend([call('start', selectors=[r['selector']]), call('read'),
        call('record', **arguments), call('read', offset=3000), call('record', **arguments),
        {'text':'Прочитаны обе части.'}])
    response=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    rows=[v['result'] for v in response['artifact']['tool_results']]
    assert rows[2]['status']=='error' and 'не полностью' in rows[2]['result']['detail']
    assert rows[1]['result']['next_cursor']=={'fragment_index':0,'offset':3000}
    assert rows[3]['result']['offset']==3000 and rows[3]['result']['next_cursor'] is None
    assert rows[1]['result']['text']+rows[3]['result']['text']==r['point']['payload']['text']
    assert response['artifact']['execution_receipt']['status']=='recorded', response['artifact']
    # The current page is mandatory; prior pages stay in the ledger, not a growing transcript.
    final=json.dumps(t.seen[-1],ensure_ascii=False)
    assert 'А😀Б'*1000 not in final and 'А😀Б'*501 in final
    with sqlite3.connect(t.state/'data/document-tasks.sqlite3') as db:
        assert db.execute('SELECT start,stop FROM review_reads ORDER BY start').fetchall()==[(0,3000),(3000,4503)]


def test_review_preserves_the_cause_of_an_unavailable_final_check_and_recovers_without_losing_decisions(review_turn):
    t=review_turn; r=t.review; call=r['call']
    def lose_index_after_generation_started(body):
        r['unavailable'].set()
        return {'text':'Решение записано.'}
    t.scripts.extend([call('start',selectors=[r['selector']]),call('read'),call('record',**decisions(t)),lose_index_after_generation_started])
    first=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    receipt=first['artifact']['execution_receipt']; task=receipt['task_id']
    assert receipt['status']=='needs_recheck' and receipt['source_check']=='unavailable'
    assert receipt['source_errors']=={r['unit']:'INDEX_READ_UNAVAILABLE'}
    assert first['artifact']['completion_status']=='incomplete'
    assert 'доступность' in receipt['notice'] and 'изменились' not in receipt['notice']
    with sqlite3.connect(t.database) as db:
        saved=json.loads(db.execute('SELECT artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()[0])
    assert saved['execution_receipt']['source_errors']==receipt['source_errors']
    r['unavailable'].clear()
    t.scripts.extend([call('status',task_id=task),{'text':'Проверка источника восстановлена.'}])
    restored=t.client.post(PATH,json={**r['request'],'question':'Проверь сохранённое задание '+task},headers=HEADERS).raise_for_status().json()
    assert restored['artifact']['execution_receipt']['status']=='recorded'
    with sqlite3.connect(t.state/'data/document-tasks.sqlite3') as db:
        assert db.execute('SELECT count(*) FROM review_events').fetchone()[0]==1


def test_review_owner_matches_existing_codec_and_cannot_cross_sessions(review_turn):
    from proxy.services.chat_profile_service import resolve_chat_profile
    from proxy.services.review_task_store import ReviewTaskStore
    t=review_turn; r=t.review; call=r['call']
    t.scripts.extend([call('start',selectors=[r['selector']]), {'text':'Список создан.'}])
    result=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    task=result['artifact']['execution_receipt']['task_id']
    profile=resolve_chat_profile(session_id=r['request']['session_id'],requested_mode=None,db_path=t.database)
    owner=digest({'session':r['request']['session_id'],'project':None,'datasets':['ds'],'scope':None,
                  'profile':profile['revision_id'],'model':t.revision.revision_id,'files':[]})
    assert ReviewTaskStore(t.state/'data/document-tasks.sqlite3').task(owner,task)['revision']==0
    sid=t.client.post('/api/workspace/sessions',json={'title':'Другой диалог'}).raise_for_status().json()['session_id']
    t.scripts.extend([call('read',task_id=task),{'text':'Не прочитано.'}])
    denied=t.client.post(PATH,json={**r['request'],'session_id':sid},headers=HEADERS)
    assert denied.status_code==503 and denied.json()['code']=='WORKSPACE_INVALID'
    # A failed source read stops before a model can invent a successful result.
    assert len(t.scripts)==1


def test_described_review_tool_is_promoted_to_an_exact_direct_schema(review_turn):
    t=review_turn; r=t.review; call=r['call']
    t.scripts.extend([{'tool':call('status')['tool'],'args':{'operation':'describe','name':'document_review'}},
        {'tool':'document_review','args':{'operation':'start','selectors':[r['selector']]}},
        {'tool':'document_review','args':{'operation':'read'}},
        {'tool':'document_review','args':{'operation':'record',**decisions(t)}}, {'text':'Записано.'}])
    first=len(t.seen)
    response=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    assert 'document_review' not in [v['function']['name'] for v in t.seen[first]['tools']]
    for message in t.seen[first+1:]:
        assert 'document_review' in [v['function']['name'] for v in message['tools']]
        assert len(message['tools'])<=3
    after_start=next(v for v in t.seen[first+2]['tools'] if v['function']['name']=='document_review')
    assert 'start' not in after_start['function']['parameters']['properties']['operation']['enum']
    assert response['artifact']['execution_receipt']['status']=='recorded'
    descriptor=response['artifact']['tool_results'][0]['result']
    assert descriptor['call_via']['arguments']['name']=='document_review'
    before_record=t.seen[first+3]['messages']
    paired=[m for m in before_record if m['role']=='tool' and m.get('name')=='document_review']
    assert len(paired)==1
    assert json.loads(paired[0]['content'])['document_state']['current_read']['text']==r['point']['payload']['text']
    assert sum(m['role']=='assistant' and any(v['function']['name']=='document_review' for v in m.get('tool_calls',[])) for m in before_record)==1


def test_model_can_select_an_exact_source_excerpt_without_retyping_it(review_turn):
    t=review_turn; r=t.review; call=r['call']
    snap=native_request('review_snapshot',dict(selector=r['selector'],allowed=['ds'],files=[],
        source={'sha256':r['checksum'],'document_id':'registered-document'},points=[r['point']]))
    fragment=native_request('review_fragment',dict(snapshot=snap,fragment_index=0,offset=0,max_chars=3000))
    selected=decisions(t); selected['evidence']=[{'read_id':fragment['read_id'],'span_id':'q1'}]
    stale={**selected,'evidence':[{'read_id':'r'+'0'*24,'span_id':'q1'}]}
    t.scripts.extend([call('start',selectors=[r['selector']]),call('read'),call('record',**stale),
                     call('record',**selected),{'text':'Вывод с точной цитатой записан.'}])
    result=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    rows=[v['result'] for v in result['artifact']['tool_results']]
    assert rows[2]['status']=='error' and rows[2]['result']['code']=='REVIEW_EXCERPT_STALE'
    assert rows[3]['status']=='ok' and result['artifact']['execution_receipt']['status']=='recorded'
    with sqlite3.connect(t.state/'data/document-tasks.sqlite3') as db:
        observation=json.loads(db.execute('SELECT observation FROM review_events').fetchone()[0])
    assert observation['evidence'][0]['quote']==r['point']['payload']['text']
    assert observation['evidence_references'][0]['kind']=='source_excerpt_selected_by_model'
    assert observation['evidence_references'][0]['offset']==0
    assert observation['evidence_references'][0]['stop']==len(r['point']['payload']['text'])


def test_provider_batch_cannot_credit_a_read_before_a_successful_model_turn(review_turn):
    t=review_turn; r=t.review; call=r['call']
    snap=native_request('review_snapshot',dict(selector=r['selector'],allowed=['ds'],files=[],
        source={'sha256':r['checksum'],'document_id':'registered-document'},points=[r['point']]))
    read=native_request('review_fragment',dict(snapshot=snap,fragment_index=0,offset=0,max_chars=3000))
    selected=decisions(t); selected['evidence']=[{'read_id':read['read_id'],'span_id':'q1'}]
    t.scripts.extend([{'calls':[call('start',selectors=[r['selector']]),call('read'),call('record',**selected)]},
        call('record',**selected), {'text':'Сохранено после передачи текста.'}])
    result=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    rows=[v['result'] for v in result['artifact']['tool_results']]
    assert rows[2]['status']=='error' and rows[2]['result']['code']=='REVIEW_QUOTE_NOT_READ'
    assert rows[3]['status']=='ok' and result['artifact']['execution_receipt']['status']=='recorded'


def test_native_review_derives_receipt_key_and_replays_the_same_decision(review_turn):
    t=review_turn; r=t.review; call=r['call']; selected=decisions(t); selected.pop('request_id')
    t.scripts.extend([call('start',selectors=[r['selector']]),call('read'),
        call('record',**selected),call('record',**selected),{'text':'Запись одна.'}])
    result=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    assert result['artifact']['execution_receipt']['revision']==1
    with sqlite3.connect(t.state/'data/document-tasks.sqlite3') as db:
        assert db.execute('SELECT count(*) FROM review_events').fetchone()[0]==1
        assert db.execute('SELECT request_id FROM review_receipts').fetchone()[0].startswith('auto:')


def test_review_read_configuration_failure_stops_before_a_confident_followup(review_turn):
    t=review_turn; r=t.review; call=r['call']
    t.scripts.extend([call('start',selectors=[r['selector']]),{'text':'Список создан.'}])
    first=t.client.post(PATH,json=r['request'],headers=HEADERS).raise_for_status().json()
    task=first['artifact']['execution_receipt']['task_id']
    (t.state/'index-contract.json').write_text('{broken',encoding='utf-8')
    t.scripts.extend([call('read',task_id=task),{'text':'SHOULD NEVER BE REQUESTED'}])
    failed=t.client.post(PATH,json={**r['request'],'question':'Прочти источник'},headers=HEADERS)
    assert failed.status_code==503 and failed.json()['code']=='INDEX_SETTINGS_INVALID'
    assert len(t.scripts)==1
    with sqlite3.connect(t.database) as db:
        artifact=json.loads(db.execute('SELECT artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()[0])
    assert artifact['tool_results'][0]['result']['operation']=='read'
    assert artifact['error']['code']=='INDEX_SETTINGS_INVALID'


def test_native_review_cache_never_bypasses_same_stat_original_hash(review_turn):
    from backend import native_core
    import os
    t=review_turn; r=t.review; call=r['call']
    journal=t.state/'storage/datasets/.index-replacements'/hashlib.sha256(b'docs').hexdigest()[:24]/'journal.sqlite'
    journal.parent.mkdir(parents=True)
    native_core.required('index_journal',request={'database':str(journal),'action':'store_sparse',
        'arguments':{'value':{'phase':'idle','revision':'cache-test-generation'}}})
    def change_before_read(body):
        old=r['path'].stat(); data=r['path'].read_bytes(); r['path'].write_bytes(b'X'+data[1:])
        os.utime(r['path'],ns=(old.st_atime_ns,old.st_mtime_ns))
        return call('read')
    t.scripts.extend([call('start',selectors=[r['selector']]),change_before_read,{'text':'SHOULD NEVER BE REQUESTED'}])
    failed=t.client.post(PATH,json=r['request'],headers=HEADERS)
    assert failed.status_code==503 and len(t.scripts)==1
    # One scroll plus its canonical point check: context preparation reused the
    # verified generation, but the changed original blocked the next read.
    assert len(r['queries'])==2
