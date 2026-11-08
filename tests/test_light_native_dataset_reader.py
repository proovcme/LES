"""Native reader queue, bounded input, revision CAS and real HTTP without application Python."""
from contextlib import closing
from datetime import datetime,timezone
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
from threading import Thread,Event
import json
import os
import sqlite3
import subprocess

import httpx
import pytest

from backend.document_catalog import MetaDB
from backend.workspace_native import catalog_call
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_contracts import CapabilityName,CapabilityObservation,CapabilitySnapshot,CapabilityState,ConnectionLocality,ConnectionRole
from tests.test_light_native_api import free_port,wait_for
from tests.test_light_native_batch_api import stop


@pytest.fixture
def case(tmp_path):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native reader core')
    database=tmp_path/'state/data/meta.db';database.parent.mkdir(parents=True)
    MetaDB(str(database))
    with sqlite3.connect(database) as db:
        db.execute("INSERT INTO datasets(id,name) VALUES('ds','Reader case')")
        db.execute("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count,file_hash,file_size,file_mtime,source_path) VALUES('doc','ds','Раздел 東京/Описание.pdf','INDEXED',3,?,10,1,'source.pdf')",('a'*64,))
    return database


def call(database,action,**args):return catalog_call(database,'reader_'+action,**args)


def output(name='Раздел 東京/Описание.pdf'):
    return {'schema':'dataset_reader_map_v1','corpus_kind':'Документы','reader_summary':'Навигация по видимым файлам.',
        'where_to_look':[{'question_type':'Содержание','target_files':[name],'reason':'Видимый источник'}],
        'file_roles':[{'file_name':name,'role':'документ','what_inside':'Раздел документа','confidence':.6}],
        'known_gaps':[],'answer_guidance':'Проверять источники.','confidence':.6}


def prepared(database,**args):
    value=call(database,'enqueue',dataset_id='ds',**args);job=call(database,'claim',auto_enabled=True)
    assert job['id']==value['job_id']
    call(database,'prepare',id=job['id'],char_limit=12000)
    return job


def test_reader_rejects_same_count_changed_document_and_preserves_pending_generation(case):
    job=prepared(case)
    with sqlite3.connect(case) as db:db.execute("UPDATE documents SET file_hash=?",('b'*64,))
    result=call(case,'finish',id=job['id'],output=json.dumps(output()))
    assert result=={'status':'stale','code':'READER_SOURCE_CHANGED'}
    assert call(case,'memory',dataset_id='ds')['reader_status']=='bootstrap'
    fresh=prepared(case)
    assert fresh['source_revision']!=job['source_revision']
    assert call(case,'finish',id=fresh['id'],output=json.dumps(output()))=={'status':'completed','code':''}
    memory=call(case,'memory',dataset_id='ds')
    assert memory['reader_status']=='model' and memory['is_evidence'] is False
    assert call(case,'enqueue',dataset_id='ds')['reason']=='ready'


@pytest.mark.parametrize('bad,code',[(output('invented.pdf'),'READER_UNKNOWN_SOURCE'),({'schema':'wrong'},'READER_OUTPUT_INVALID'),('broken json','READER_OUTPUT_INVALID')])
def test_reader_invalid_output_never_publishes_success(case,bad,code):
    job=prepared(case);raw=bad if isinstance(bad,str) else json.dumps(bad)
    assert call(case,'validate',id=job['id'],output=raw)=={'valid':False,'code':code}
    assert call(case,'finish',id=job['id'],output=raw)=={'status':'failed','code':code}
    value=call(case,'memory',dataset_id='ds')
    assert value['reader_status']=='model_failed' and value['reader_output'] is None and value['reader_errors']==[code]
    assert value['reader_error_messages']


def test_reader_bounded_input_remains_valid_json_with_unicode_and_omissions(case):
    with sqlite3.connect(case) as db:
        for i in range(150):
            db.execute("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count,file_hash) VALUES(?,'ds',?,'INDEXED',1,'hash')",
                (f'doc{i}',f'Группа {i}/東京 '+('Длинное имя '*8)+f'{i}.pdf'))
    job=prepared(case)
    value=call(case,'prepare',id=job['id'],char_limit=1800)
    data=value['input'];serialized=json.dumps(data,ensure_ascii=False,separators=(',',':'))
    assert len(serialized)<=1800 and json.loads(serialized)==data
    assert data['file_cards_scope']['truncated'] and data['file_cards_scope']['included']<data['file_cards_scope']['total']==151
    known={item['file_name'] for item in data['file_cards']}
    omitted=next(name for (name,) in sqlite3.connect(case).execute('SELECT file_name FROM documents') if name not in known)
    assert call(case,'validate',id=job['id'],output=json.dumps(output(omitted)))=={'valid':False,'code':'READER_UNKNOWN_SOURCE'}


def test_reader_reconciliation_closes_crash_gap_and_disable_pauses_auto_jobs(case):
    with sqlite3.connect(case) as db:
        db.execute('CREATE TABLE les_index_events(id TEXT PRIMARY KEY,dataset_id TEXT,kind TEXT,created_at REAL)')
        db.execute("INSERT INTO les_index_events VALUES('committed-before-kill','ds','index_completed',1)")
    assert call(case,'reconcile')=={'checked':1}
    assert call(case,'reconcile')=={'checked':0}
    assert call(case,'claim',auto_enabled=False) is None
    job=call(case,'claim',auto_enabled=True)
    assert job['origin']=='auto'
    call(case,'bind',id=job['id'],revision_id='pinned-model')
    assert call(case,'recover')=={'recovered':1}
    resumed=call(case,'claim',auto_enabled=True)
    assert resumed['id']==job['id'] and resumed['model_revision']=='pinned-model' and resumed['recoveries']==1
    call(case,'prepare',id=job['id'])
    assert call(case,'finish',id=job['id'],output=json.dumps(output()))['status']=='completed'


def test_reader_large_name_inventory_has_bounded_memory_projection(case):
    with sqlite3.connect(case) as db:
        for i in range(1000):
            name=f'Группа {i}/'+'/'.join(['я'*80]*12)+f'/file{i}.pdf'
            db.execute("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count,file_hash) VALUES(?,'ds',?,'INDEXED',1,'hash')",(f'long{i}',name))
    memory=call(case,'memory',dataset_id='ds')
    assert memory['document_count']==1001
    assert len(memory['file_cards'])<500
    assert memory['file_cards_scope']=={'included':len(memory['file_cards']),'total':1001}
    assert len(json.dumps(memory,ensure_ascii=False).encode())<2*1024*1024


def test_reader_does_not_recreate_deleted_dataset(case):
    job=prepared(case)
    with sqlite3.connect(case) as db:db.execute("DELETE FROM datasets WHERE id='ds'")
    assert call(case,'finish',id=job['id'],output=json.dumps(output()))=={'status':'stale','code':'READER_DATASET_REMOVED'}


def test_intake_completion_emits_event_when_last_document_already_committed(case):
    job=catalog_call(case,'intake_enqueue',root='source',dataset_id='ds',parse_limit=5)['id']
    catalog_call(case,'intake_claim')
    catalog_call(case,'intake_manifest',id=job,root='source',files=[{'file_name':'Раздел 東京/Описание.pdf','source_path':'source.pdf'}])
    batch=catalog_call(case,'intake_batch',id=job)
    catalog_call(case,'intake_register',id=job,files=[{**file,'file_size':10,'file_mtime':1.} for file in batch])
    assert catalog_call(case,'intake_index_batch',id=job)==[]
    checkpoint=catalog_call(case,'intake_index_checkpoint',id=job,reason='',message='')
    assert checkpoint['status']=='completed'
    assert call(case,'reconcile')=={'checked':1}
    queued=call(case,'claim',auto_enabled=True)
    assert queued['dataset_id']=='ds' and queued['origin']=='auto'


def test_reader_retains_repair_cause_and_attempt_budget_after_restart(case):
    job=prepared(case)
    call(case,'bind',id=job['id'],revision_id='pinned')
    call(case,'attempt',id=job['id'])
    call(case,'retry',id=job['id'],code='READER_OUTPUT_INVALID',output='invalid first answer')
    call(case,'recover')
    resumed=call(case,'claim')
    assert resumed['attempts']==1 and resumed['attempts_limit']==2
    assert resumed['model_revision']=='pinned' and resumed['repair_code']=='READER_OUTPUT_INVALID'
    assert resumed['repair_output']=='invalid first answer'
    call(case,'prepare',id=job['id'])
    raw='```json\n'+json.dumps({**output(),'reader_summary':'я'*1000})+'\n```'
    assert call(case,'finish',id=job['id'],output=raw)['status']=='completed'
    memory=call(case,'memory',dataset_id='ds')
    assert len(memory['reader_output']['reader_summary'])==900


def test_reader_corrupt_snapshot_fails_without_panicking(case):
    job=call(case,'enqueue',dataset_id='ds')
    with sqlite3.connect(case) as db:db.execute("UPDATE les_dataset_reader_jobs SET snapshot_json='{}' WHERE id=?",[job['job_id']])
    claimed=call(case,'claim')
    with pytest.raises(sqlite3.OperationalError,match='Повреждена карта'):
        call(case,'prepare',id=claimed['id'])
    call(case,'finish',id=claimed['id'],error_code='READER_STORAGE_INVALID')
    assert call(case,'memory',dataset_id='ds')['reader_status']=='model_failed'


def test_legacy_projection_cannot_keep_unverified_reader_answer(case,monkeypatch):
    from backend import native_index_store
    from proxy.services import dataset_memory_service as memory
    monkeypatch.setattr(native_index_store,'_bridge',('http://127.0.0.1:1/api/light/internal-index-store','0'*64))
    monkeypatch.setattr(memory,'rag_meta_db_path',lambda:str(case))
    legacy={'dataset_id':'ds','reader_status':'model','reader_output':{'reader_summary':'Old unpinned map'},'custom_navigation':'keep'}
    projected=memory._reader_overlay(legacy,'ds',str(case))
    assert projected['reader_status']=='bootstrap' and projected['reader_output'] is None
    assert projected['custom_navigation']=='keep'


def test_native_reader_http_repair_failure_disconnect_and_crash_resume(case,tmp_path,monkeypatch):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Build native reader API')
    state=case.parent.parent;requests=[];mode={'value':'valid'};arrived=Event();release=Event();release.set()
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append(value);assert value['model']=='reader-fixture'
            if mode['value']=='pause':arrived.set();release.wait(30)
            reply=output('invented.pdf') if mode['value']=='hallucinate' else output()
            if mode['value']=='repair' and 'Исправь ответ:' not in value['messages'][0]['content']:reply={'bad':'first output'}
            payload={'model':'reader-fixture','choices':[{'message':{'content':json.dumps(reply,ensure_ascii=False)},'finish_reason':'stop'}]}
            raw=json.dumps(payload,ensure_ascii=False).encode()
            try:
                self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
            except OSError:pass
    model=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=model.serve_forever,daemon=True);thread.start()
    registry=ModelConnectionRegistry(str(case))
    revision=registry.create_connection(display_name='Reader test',base_url=f'http://127.0.0.1:{model.server_port}/v1',model_id='reader-fixture',
        locality=ConnectionLocality.LOOPBACK,requested_context_tokens=24000,secret_ref=None,extension_type=None,actor='admin:test')
    observed=datetime(2026,1,1,tzinfo=timezone.utc)
    registry.save_capability_snapshot(CapabilitySnapshot('cap:reader',revision.revision_id,
        (CapabilityObservation(CapabilityName.CHAT_COMPLETIONS,CapabilityState.SUPPORTED,'probe',observed),),observed,
        datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'24000'}),actor='admin:test')
    registry.bind_role(ConnectionRole.ANSWER,revision.revision_id,expected_binding_revision=None,actor='admin:test')
    policy=state/'policy.json';policy.write_text(json.dumps({'schema':'les.native-auth-policy.v1','compatible':True,'trusted_role':'admin',
        'proxy_header':'x-les-role','cors_origins':[],'cors_loopback':True}),encoding='utf-8')
    (state/'.env').write_text('',encoding='utf-8');port=free_port();upstream=free_port();token='6'*64;api=None
    def launch():
        env={**os.environ,'LES_LIGHT_QDRANT_URL':'http://127.0.0.1:1','LES_LIGHT_QDRANT_API_KEY':'reader-no-storage-access',
            'LES_INTERNAL_INGEST_TOKEN':token,'LES_STARTUP_BACKGROUND_MUTATIONS':'true','LES_ENV_PATH':str(state/'.env'),
            'PYTHONHOME':str(tmp_path/'no-python'),'PYTHONPATH':str(tmp_path/'no-application'),'PATH':str(tmp_path/'no-programs')}
        with (tmp_path/'api.log').open('ab') as log:
            process=subprocess.Popen([program,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{upstream}',
                '--state',str(state),'--database',str(case),'--auth-policy-file',str(policy)],env=env,stdout=log,stderr=log,
                creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        wait_for(lambda:httpx.get(f'http://127.0.0.1:{port}/api/notebooks/ds/memory/reader',trust_env=False).status_code==200)
        return process
    try:
        api=launch()
        with httpx.Client(base_url=f'http://127.0.0.1:{port}',timeout=30,trust_env=False) as client:
            endpoint='/api/notebooks/ds/memory/read'
            assert client.post('/api/light/internal-dataset-reader',json={'dataset_id':'ds'}).status_code==404
            assert client.post('/api/light/internal-dataset-reader',json={'dataset_id':'ds'},headers={'x-les-index-token':token,'Origin':str(client.base_url)}).status_code==404
            mode['value']='repair'
            response=client.post(endpoint,json={}).raise_for_status();value=response.json()
            assert response.headers['x-les-handler']=='rust.dataset-reader'
            assert value['reader_status']=='model' and value['reader_attempts']==2 and len(requests)==2
            assert value['is_evidence'] is False
            assert client.post(endpoint,json={}).raise_for_status().json()['reader_status']=='model' and len(requests)==2
            from backend import native_index_store
            from proxy.services import dataset_memory_service as memory
            import asyncio
            monkeypatch.setattr(native_index_store,'_bridge',(str(client.base_url).rstrip('/')+'/api/light/internal-index-store',token))
            def prohibited(*args,**kwargs):raise AssertionError('Python reader preparation invoked')
            monkeypatch.setattr(memory,'build_typed_dataset_memory',prohibited)
            assert asyncio.run(memory.run_dataset_reader_pass('ds'))['reader_status']=='model'
            mode['value']='hallucinate'
            failed=client.post(endpoint,json={'force':True}).raise_for_status().json()
            assert failed['reader_status']=='model_failed' and failed['reader_errors']==['READER_UNKNOWN_SOURCE'] and failed['reader_output'] is None
            mode['value']='pause';arrived.clear();release.clear()
            job=client.post(endpoint,json={'force':True,'background':True}).raise_for_status().json()['job_id']
            assert arrived.wait(10)
            assert client.get('/api/notebooks/ds/memory/reader').raise_for_status().json()['reader_status']=='running'
            stop(api)
            newer=registry.revise_connection(revision.connection_id,expected_revision_id=revision.revision_id,actor='admin:test',model_id='unselected-model')
            registry.bind_role(ConnectionRole.ANSWER,newer.revision_id,expected_binding_revision=1,actor='admin:test')
            mode['value']='valid';release.set();api=launch()
            wait_for(lambda:call(case,'job',id=job)['status']=='completed',timeout=20)
            recovered=call(case,'job',id=job)
            assert recovered['recoveries']==1 and recovered['model_revision']==revision.revision_id
            assert recovered['error_code']=='' and 'snapshot_json' not in recovered
            registry.bind_role(ConnectionRole.ANSWER,revision.revision_id,expected_binding_revision=2,actor='admin:test')
            # The model/queue remain owned by Rust when the synchronous caller disappears.
            mode['value']='pause';arrived.clear();release.clear()
            with httpx.Client(base_url=str(client.base_url),timeout=.2,trust_env=False) as impatient:
                with pytest.raises(httpx.ReadTimeout):impatient.post(endpoint,json={'force':True})
            assert arrived.wait(10);mode['value']='valid';release.set()
            wait_for(lambda:client.get('/api/notebooks/ds/memory/reader').json()['reader_status']=='model',timeout=20)
            # A committed outbox event is sufficient: notification and Python are absent.
            with sqlite3.connect(case) as db:
                db.execute("UPDATE documents SET file_hash=?",('c'*64,))
                db.execute('CREATE TABLE IF NOT EXISTS les_index_events(id TEXT PRIMARY KEY,dataset_id TEXT,kind TEXT,created_at REAL)')
                db.execute("INSERT INTO les_index_events VALUES('committed-before-notify','ds','index_completed',1)")
            (state/'.env').write_text('LES_DATASET_READER_AFTER_PARSE=true\n',encoding='utf-8')
            wait_for(lambda:client.get('/api/notebooks/ds/memory/reader').json()['reader_status']=='model',timeout=20)
            with sqlite3.connect(case) as db:
                assert db.execute("SELECT count(*) FROM les_dataset_reader_jobs WHERE origin='auto' AND status='completed'").fetchone()==(1,)
    finally:
        release.set();stop(api);model.shutdown();model.server_close();thread.join(timeout=5)
