"""Real native first-index controller; storage faults must never erase live points."""
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from threading import Thread,Event
from types import SimpleNamespace
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time

import httpx
import pytest
from backend.document_catalog import MetaDB
from backend.index_replacement import ReplacementJournal
from tests.native_batch_fixtures import NativeBatchEmbeddings
from tests.test_light_native_api import free_port,wait_for
from tests.test_light_native_batch_api import stop


@pytest.fixture
def collection(tmp_path):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program or not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native API and core')
    state=tmp_path/'state';database=state/'data/meta.db';database.parent.mkdir(parents=True)
    MetaDB(str(database))
    (state/'.env').write_text('RAG_COLLECTION_NAME=docs\nRAG_CHUNK_UNIT=chars\nLES_RAG_PAYLOAD_INDEX_ENSURE=false\n',encoding='utf-8')
    model=NativeBatchEmbeddings(database)
    storage=SimpleNamespace(exists=False,summary=0,points=[],matched=0,compatible=True,get_failure=False,count_failure=False,calls=[],pause=False,arrived=Event(),release=Event())
    def params():
        return {'vectors':{'dense':{'size':2,'distance':'Cosine'}} if storage.compatible else {'size':2,'distance':'Cosine'},
                'sparse_vectors':{'bm25_sparse':{}} if storage.compatible else None}
    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def reply(self,status,value):
            body=json.dumps(value).encode();self.send_response(status);self.send_header('Content-Length',str(len(body)));self.end_headers()
            try:self.wfile.write(body)
            except OSError:pass
        def do_GET(self):
            storage.calls.append('get')
            if storage.get_failure:self.reply(503,{});return
            if not storage.exists:self.reply(404,{});return
            self.reply(200,{'status':'ok','result':{'points_count':storage.summary,'config':{'params':params()}}})
        def do_POST(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert self.path.endswith('/points/count') and value['exact'] is True
            storage.calls.append('count')
            if storage.count_failure:self.reply(503,{});return
            self.reply(200,{'status':'ok','result':{'count':storage.matched if value.get('filter') else len(storage.points)}})
        def do_PUT(self):
            value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert value['vectors']['dense']['size']==2
            storage.calls.append('create');storage.exists=True;storage.compatible=True
            if storage.pause:storage.arrived.set();storage.release.wait(20)
            self.reply(200,{'status':'ok','result':True})
        def do_DELETE(self):
            storage.calls.append('delete');storage.exists=False;storage.points.clear()
            self.reply(200,{'status':'ok','result':True})
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler);thread=Thread(target=server.serve_forever,daemon=True);thread.start()
    port,upstream=free_port(),free_port();token='a'*64
    environment={k:v for k,v in os.environ.items() if not k.startswith(('RAG_','EMBED','COREML_EMBED_','TRUSTED_','CORS_')) and k not in ['LES_NATIVE_EMBEDDING_SPEC','LES_ENV_PATH','LES_EMBED_PROFILE','BGE_MODEL']}
    environment.update(LES_LIGHT_QDRANT_URL=f'http://127.0.0.1:{server.server_port}',LES_LIGHT_QDRANT_API_KEY='setup-test',
        LES_INTERNAL_INGEST_TOKEN=token,LES_ENV_PATH=str(state/'.env'),LES_STARTUP_BACKGROUND_MUTATIONS='false',
        PYTHONHOME=str(tmp_path/'no-python'),PYTHONPATH=str(tmp_path/'no-app'),PATH=str(tmp_path/'no-programs'))
    command=[program,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{upstream}','--state',str(state),'--database',str(database)]
    process=None
    with (tmp_path/'api.log').open('ab') as log,httpx.Client(base_url=f'http://127.0.0.1:{port}',trust_env=False,timeout=20) as client:
        def start():
            nonlocal process
            process=subprocess.Popen(command,env=environment,stdout=log,stderr=log,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
            wait_for(lambda:client.get('/api/rag/index/setup').status_code==200)
        start()
        case=SimpleNamespace(state=state,database=database,storage=storage,model=model,client=client,start=start,stop=lambda:stop(process),command=command,environment=environment,
            contract=database.with_name(database.name+'.docs.index-contract.json'))
        try:yield case
        finally:
            storage.release.set();stop(process);model.close();server.shutdown();server.server_close();thread.join(timeout=5)


def setup(case,**body):return case.client.post('/api/rag/index/setup',json=body)


@pytest.mark.parametrize('reported',[0,None])
def test_zero_or_unknown_summary_cannot_authorize_deleting_real_points(collection,reported):
    case=collection;s=case.storage;s.exists=True;s.compatible=False;s.points=['preserve'];s.summary=reported
    result=setup(case,adopt_existing=True,replace_empty=True)
    assert result.json()['code']=='INDEX_SCHEMA_MIGRATION_REQUIRED'
    assert s.points==['preserve'] and 'delete' not in s.calls and not case.contract.exists()


def test_unavailable_exact_count_preserves_the_collection(collection):
    case=collection;case.storage.exists=True;case.storage.count_failure=True
    assert setup(case,adopt_existing=True).status_code==503
    assert 'delete' not in case.storage.calls and not case.contract.exists()


def test_empty_legacy_migration_requires_explicit_confirmation(collection):
    case=collection;s=case.storage;s.exists=True;s.compatible=False
    assert setup(case).json()['code']=='INDEX_ADOPTION_REQUIRED'
    assert 'delete' not in s.calls
    assert setup(case,adopt_existing=True).json()['code']=='INDEX_SCHEMA_MIGRATION_REQUIRED'
    result=setup(case,adopt_existing=True,replace_empty=True).raise_for_status().json()
    assert result['created'] and s.calls.index('count')<s.calls.index('delete')<s.calls.index('create')
    assert case.contract.is_file()


def test_compatibility_does_not_adopt_unverified_points_as_empty(collection):
    case=collection;s=case.storage;s.exists=True;s.points=['preserve'];s.matched=0
    assert setup(case,adopt_existing=True).json()['code']=='INDEX_ADOPTION_UNVERIFIED'
    assert s.points==['preserve'] and not case.contract.exists()


def test_startup_cannot_race_a_publication_or_recovery_writer(collection):
    case=collection
    with ReplacementJournal(case.state/'storage/datasets','docs').lease():
        assert setup(case).json()['code']=='INDEX_UPDATE_BUSY'
    assert 'create' not in case.storage.calls


def test_get_error_does_not_turn_into_create(collection):
    case=collection;case.storage.get_failure=True
    assert setup(case).status_code==503
    assert 'create' not in case.storage.calls


def test_real_404_creates_and_publishes_a_native_identity(collection,monkeypatch):
    case=collection;result=setup(case).raise_for_status()
    assert result.headers['x-les-handler']=='rust.index-setup' and result.json()['vector_size']==2
    value=json.loads(case.contract.read_text())
    unsigned={k:v for k,v in value.items() if k!='fingerprint'}
    assert hashlib.sha256(json.dumps(unsigned,ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()).hexdigest()==value['fingerprint']
    assert len(case.model.calls)==1
    assert not setup(case).raise_for_status().json()['created'] and len(case.model.calls)==1
    assert case.client.get('/api/rag/index/setup').json()['phase']=='completed'
    from backend import rag_config
    for key in ['EMBEDDING_MODEL','RAG_VECTOR_SIZE','EMBED_BACKEND','EMBED_MODEL']:monkeypatch.delenv(key,raising=False)
    monkeypatch.setenv('RAG_COLLECTION_NAME','docs');monkeypatch.setenv('RAG_CHUNK_UNIT','chars');monkeypatch.setenv('RAG_INDEX_CONTRACT_PATH',str(case.contract))
    assert rag_config.index_contract_status()['compatible']


@pytest.mark.parametrize('missing_collection',[False,True])
def test_missing_index_cannot_be_silently_recreated_over_indexed_catalog(collection,missing_collection):
    case=collection;s=case.storage;s.exists=not missing_collection
    with sqlite3.connect(case.database) as db:
        db.execute("INSERT INTO datasets(id,name) VALUES('ds','Catalog fixture')")
        db.execute("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count) VALUES('doc','ds','source.txt','INDEXED',1)")
    assert setup(case,adopt_existing=True).json()['code']=='INDEX_CATALOG_MISMATCH'
    assert 'create' not in s.calls and 'delete' not in s.calls
    with sqlite3.connect(case.database) as db:assert db.execute('SELECT status,chunk_count FROM documents').fetchone()==('INDEXED',1)


def test_crash_after_collection_creation_resumes_the_plan_without_adoption_or_second_probe(collection):
    case=collection;s=case.storage;s.pause=True
    with ThreadPoolExecutor(1) as pool:
        future=pool.submit(setup,case)
        assert s.arrived.wait(10)
        assert case.client.get('/api/rag/index/setup').json()['phase']=='planned'
        assert not case.contract.exists()
        case.stop()
        with pytest.raises(httpx.HTTPError):future.result()
        s.release.set();s.pause=False
    case.start()
    result=setup(case).raise_for_status().json()
    assert result['resumed'] and not result['created'] and len(case.model.calls)==1
    assert s.calls.count('create')==1 and case.contract.is_file()


def test_missing_role_never_creates_storage(collection):
    case=collection
    with sqlite3.connect(case.database) as db:db.execute("DELETE FROM les_model_role_bindings WHERE role='embeddings'")
    assert setup(case).json()['code']=='EMBEDDING_MODEL_NOT_ASSIGNED'
    assert not case.model.calls and 'create' not in case.storage.calls
    assert case.client.get('/api/rag/index/setup').json()['error_code']=='EMBEDDING_MODEL_NOT_ASSIGNED'


def test_dimension_mismatch_never_creates_storage(collection):
    case=collection
    with (case.state/'.env').open('a',encoding='utf-8') as file:file.write('RAG_VECTOR_SIZE=3\n')
    assert setup(case).json()['code']=='EMBEDDING_INDEX_DIMENSION_MISMATCH'
    assert len(case.model.calls)==1 and 'create' not in case.storage.calls and not case.contract.exists()


def test_corrupt_contract_is_preserved_and_not_adopted(collection):
    case=collection;case.contract.write_bytes(b'not json')
    assert setup(case).json()['code']=='INDEX_CONTRACT_INVALID'
    assert case.contract.read_bytes()==b'not json' and not case.storage.calls and not case.model.calls


def test_changed_role_during_probe_leaves_storage_uncreated(collection):
    case=collection
    def change_role(_):
        with sqlite3.connect(case.database) as db:db.execute("DELETE FROM les_model_role_bindings WHERE role='embeddings'")
    case.model.on_request=change_role
    result=setup(case)
    # The plan may be durable, but publication must recheck role ownership.
    assert result.status_code==503 and not case.contract.exists() and 'create' not in case.storage.calls
    assert case.client.get('/api/rag/index/setup').json()['error_code']=='EMBEDDING_MODEL_NOT_ASSIGNED'


def test_private_setup_rejects_browser_and_untrusted_authority(collection):
    case=collection;path='/api/light/internal-index-setup'
    assert case.client.post(path,json={}).status_code==404
    assert case.client.post(path,json={},headers={'Origin':str(case.client.base_url).rstrip('/'),'X-Les-Index-Token':'a'*64}).status_code==404
    assert case.client.post(path,json={'database':'outside.db'},headers={'X-Les-Index-Token':'a'*64}).status_code==422
    assert not case.storage.calls


def test_unicode_collection_name_preserves_its_identity(collection):
    case=collection
    with (case.state/'.env').open('a',encoding='utf-8') as file:file.write("RAG_COLLECTION_NAME='Лес №1'\n")
    value=setup(case).raise_for_status().json()
    assert value['collection']=='Лес №1' and value['vector_size']==2


def test_contract_publication_rejects_tampering_and_never_overwrites(collection):
    from backend import native_core
    case=collection;setup(case).raise_for_status()
    contract=json.loads(case.contract.read_text(encoding='utf-8'))
    target=case.state/'publication.json'
    changed=dict(contract,vector_size=99)
    with pytest.raises(RuntimeError,match='INDEX_CONTRACT_INVALID'):
        native_core.required('index_contract_publish',request={'path':str(target),'contract':changed})
    assert not target.exists()
    native_core.required('index_contract_publish',request={'path':str(target),'contract':contract})
    published=target.read_bytes()
    with pytest.raises(RuntimeError,match='INDEX_CONTRACT_EXISTS'):
        native_core.required('index_contract_publish',request={'path':str(target),'contract':contract})
    assert target.read_bytes()==published


def test_native_lexical_migration_preserves_existing_text_and_repairs_fts(tmp_path):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native core')
    from backend import native_core
    path=tmp_path/'legacy.sqlite'
    with sqlite3.connect(path) as db:
        db.executescript("CREATE TABLE lexical_chunks(id INTEGER PRIMARY KEY,collection TEXT NOT NULL,point_id TEXT NOT NULL,dataset_id TEXT,doc_id TEXT,doc_name TEXT,text TEXT NOT NULL,content_hash TEXT NOT NULL,chunk_ord INTEGER,section_heading TEXT,updated_at REAL NOT NULL,UNIQUE(collection,point_id));")
        db.execute("INSERT INTO lexical_chunks VALUES(1,'docs','p1','ds','doc','Название','Проводка через перегородку','hash',1,'Раздел',0)")
    for _ in range(2):native_core.required('lexical_initialize',request={'database':str(path)})
    with sqlite3.connect(path) as db:
        assert db.execute("SELECT text FROM lexical_chunks WHERE id=1").fetchone()==('Проводка через перегородку',)
        assert db.execute("SELECT count(*) FROM lexical_chunks_fts WHERE lexical_chunks_fts MATCH 'Проводка'").fetchone()==(1,)
        db.execute("UPDATE lexical_chunks SET text='Кабель' WHERE id=1")
        assert db.execute("SELECT count(*) FROM lexical_chunks_fts WHERE lexical_chunks_fts MATCH 'Кабель'").fetchone()==(1,)
        assert db.execute("SELECT count(*) FROM lexical_chunks_fts WHERE lexical_chunks_fts MATCH 'Проводка'").fetchone()==(0,)


def test_native_edge_initialization_survives_death_before_final_catalog_commit(collection):
    from qdrant_client import QdrantClient
    case=collection;case.stop()
    edge=case.state/'edge'
    case.command.extend(['--edge-storage',str(edge)])
    case.environment['LES_LIGHT_QDRANT_URL']=str(case.client.base_url).rstrip('/')
    case.start()
    lock=None
    with ThreadPoolExecutor(1) as pool:
        future=pool.submit(setup,case)
        try:
            deadline=time.monotonic()+10
            while time.monotonic()<deadline:
                probe=sqlite3.connect(case.database,timeout=.05)
                try:
                    row=probe.execute("SELECT phase FROM les_index_setup WHERE collection='docs'").fetchone()
                    if row==('planned',):
                        probe.execute('BEGIN IMMEDIATE');lock=probe;break
                except sqlite3.OperationalError:pass
                probe.close();time.sleep(.001)
            assert lock is not None,'Failed to intercept the native final checkpoint'
            wait_for(lambda:case.contract.exists(),timeout=5)
            assert case.client.get('/api/rag/index/setup').json()['phase']=='planned'
            case.stop()
            with pytest.raises(httpx.HTTPError):future.result()
        finally:
            if lock is not None:lock.rollback();lock.close()
    case.start()
    result=setup(case).raise_for_status().json()
    assert result['resumed'] and not result['created'] and len(case.model.calls)==1
    vectors=QdrantClient(url=str(case.client.base_url),api_key='setup-test',check_compatibility=False)
    try:
        assert vectors.get_collection('docs').config.params.vectors['dense'].size==2
        assert vectors.count('docs',exact=True).count==0
    finally:vectors.close()
