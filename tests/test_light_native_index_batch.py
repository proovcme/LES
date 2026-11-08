"""Native batch control: actual catalog, source generations, process death and IO boundary faults."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time

import pytest

from backend import native_core
from backend.document_catalog import MetaDB


def make_case(root, names=('Первый 東京.txt',)):
    root=Path(root); database=root/'data/meta.db'; content=root/'storage/datasets'
    (content/'ds').mkdir(parents=True)
    spools=root/'data/index-publish';spools.mkdir(parents=True)
    MetaDB(str(database))
    with sqlite3.connect(database) as conn:
        conn.execute("INSERT INTO datasets(id,name) VALUES('ds','Case')")
        for i,name in enumerate(names):
            source=content/'ds'/name;source.write_text('Источник '+str(i)+' — независимое содержимое документа',encoding='utf-8')
            stat=source.stat()
            conn.execute("INSERT INTO documents(id,dataset_id,file_name,status,source_path,file_mtime,file_size) VALUES(?,'ds',?,'PENDING','',?,?)",
                ('doc-'+str(i),name,stat.st_mtime,stat.st_size))
    return {'schema':'les.index-batch.v1','database':str(database),'content_root':str(content),
        'spool_directory':str(spools),'dataset_id':'ds','collection':'docs','limit':100,
        'embedding':{'vector_size':'2','backend':'fixture','model_id':'fixture'},
        'embedding_fingerprint':'fixture','vector_names':None,'embed_batch':2,'upsert_batch':2,
        'cache_enabled':False,'verify_every':1,'max_attempts':4,'intake_id':None}


class Boundary:
    """Test IO implementation, never used by the product or the skill interpreter."""
    def __init__(self,request,*,convert=None,publish=None):
        self.request=request;self.convert=convert;self.publish=publish;self.events=[];self.saved=[]
    def __call__(self,operation,value):
        self.events.append((operation,value.copy() if isinstance(value,dict) else value))
        if operation==0:
            if self.convert:
                overridden=self.convert(value)
                if overridden is not None:return overridden
            text=Path(value['path']).read_text(encoding='utf-8')
            node={'text':text,'doc_id':'node-'+value['file_name'],'payload':{'content_hash':hashlib.sha1(text.encode()).hexdigest()}}
            Path(value['node_spool']).write_text(json.dumps(node,ensure_ascii=False)+'\n',encoding='utf-8')
            return {'count':1,'route':{'doc_type':'TEXT','content_type':'text','domain':'','complexity':'simple','pipeline':'markdown'}}
        if operation==1:
            return [[1.,0.] for _ in value]
        assert operation==2
        if value['action']=='recover_file':return {'status':'idle'}
        assert value['action']=='publish_file'
        if self.publish:
            overridden=self.publish(value)
            if overridden is not None:return overridden
        self.commit(value)
        return {'status':'completed','points':len(value['ids'])}
    def commit(self,value):
        points=[json.loads(line) for line in (Path(self.request['spool_directory'])/(value['spool_id']+'.jsonl')).read_text(encoding='utf-8').splitlines()]
        assert [point['id'] for point in points]==value['ids']
        assert all(point['payload']['source_sha256']==value['fingerprint']['file_hash'] for point in points)
        self.saved+=points
        with sqlite3.connect(self.request['database']) as conn:
            conn.execute("UPDATE documents SET status='INDEXED',stage='',file_hash=?,chunk_count=? WHERE id=?",
                (value['fingerprint']['file_hash'],len(points),value['registration'][0]))


@pytest.fixture(autouse=True)
def required_core():
    if not native_core.supports('index_run'):pytest.skip('Build native batch owner')


def rows(request):
    with sqlite3.connect(request['database']) as conn:
        conn.row_factory=sqlite3.Row
        return [dict(row) for row in conn.execute('SELECT * FROM documents ORDER BY file_name')]


def test_native_batch_owns_order_stage_counts_and_complete_original_binding(tmp_path):
    request=make_case(tmp_path,('B.txt','А.txt'))
    before={path:path.read_bytes() for path in (Path(request['content_root'])/'ds').glob('*')}
    io=Boundary(request)
    result=native_core.run_index_batch(request,io)
    assert (result['owner'],result['errors'],result['files_parsed'],result['chunks'],result['remaining_pending'])==('rust.index-batch',0,2,2,0)
    assert all(row['status']=='INDEXED' and row['parse_attempts']==1 and row['stage']=='' for row in rows(request))
    assert all(path.read_bytes()==value for path,value in before.items())
    assert not list(Path(request['spool_directory']).iterdir())
    with sqlite3.connect(request['database']) as conn:
        assert conn.execute("SELECT status,chunk_count FROM datasets").fetchone()==('COMPLETED',2)
        assert conn.execute("SELECT status,stop_code,spool_id FROM les_index_batches").fetchone()==('completed','','')
        assert conn.execute("SELECT count(*) FROM les_index_events e JOIN les_index_batches b ON e.id=b.id WHERE e.kind='index_completed' AND e.dataset_id='ds' AND b.status='completed'").fetchone()==(1,)


def test_replaced_registration_is_never_marked_failed_by_stale_attempt(tmp_path):
    request=make_case(tmp_path)
    def replace(value):
        with sqlite3.connect(request['database']) as conn:
            conn.execute("UPDATE documents SET id='replacement',parse_attempts=0,stage='',last_error='' WHERE id='doc-0'")
        return {'error':{'code':'FORMAT_READ_FAILED','message':'old failed','error_type':'DocumentExtractionError'}}
    result=native_core.run_index_batch(request,Boundary(request,convert=replace))
    row,=rows(request)
    assert result['errors']==1
    assert (row['id'],row['status'],row['parse_attempts'],row['last_error'])==('replacement','PENDING',0,'')
    assert result['file_errors'][0]['error_code']=='FORMAT_READ_FAILED'
    with sqlite3.connect(request['database']) as conn:
        saved=json.loads(conn.execute('SELECT result FROM les_index_batches').fetchone()[0])
        assert saved['file_errors'][0]['message']=='old failed'


@pytest.mark.parametrize('code,maximum,retryable',[
    ('WORKER_TIMEOUT',4,1),('WORKER_TIMEOUT',1,0),('FORMAT_WORKER_UNAVAILABLE',4,0),
    ('NO_SEARCHABLE_TEXT',4,0),('EMBEDDING_INDEX_MODEL_MISMATCH',4,0),
])
def test_native_retry_budget_and_terminal_model_errors_are_persisted(tmp_path,code,maximum,retryable):
    request=make_case(tmp_path);request['max_attempts']=maximum
    io=Boundary(request,convert=lambda _:{'error':{'code':code,'message':'private diagnostic','error_type':'FormatProcessError'}})
    result=native_core.run_index_batch(request,io)
    row,=rows(request)
    assert (row['error_code'],row['retryable'],row['parse_attempts'])==(code,retryable,1)
    assert result['file_errors'][0]['error_code']==code and result['file_errors'][0]['message']=='private diagnostic'
    assert not any(operation==1 for operation,_ in io.events)


def test_invalid_frozen_dimension_never_claims_or_changes_dataset(tmp_path):
    request=make_case(tmp_path);request['embedding']['vector_size']='wrong'
    with pytest.raises(native_core.NativeCoreError,match='EMBEDDING_DESCRIPTOR_INVALID'):
        native_core.run_index_batch(request,Boundary(request))
    row,=rows(request)
    assert (row['status'],row['parse_attempts'])==('PENDING',0)
    with sqlite3.connect(request['database']) as conn:
        assert not conn.execute("SELECT 1 FROM sqlite_master WHERE name='les_index_batches'").fetchone()


@pytest.mark.parametrize('status',['legacy','failed','unknown',None])
def test_unconfirmed_recovery_never_starts_preparation(tmp_path,status):
    request=make_case(tmp_path)
    io=Boundary(request)
    def exchange(operation,value):
        if operation==2 and value['action']=='recover_file':return {'status':status}
        return io(operation,value)
    result=native_core.run_index_batch(request,exchange)
    assert result['status']=='failed' and result['error']=='INDEX_RECOVERY_INCOMPLETE'
    assert rows(request)[0]['parse_attempts']==0 and not io.events


def test_successful_subset_does_not_hide_existing_dataset_errors(tmp_path):
    request=make_case(tmp_path,('A.txt','B.txt'))
    request['sources']=[{'file_name':'A.txt','source_path':''}]
    with sqlite3.connect(request['database']) as conn:conn.execute("UPDATE documents SET status='ERROR',error_code='NO_SEARCHABLE_TEXT' WHERE file_name='B.txt'")
    result=native_core.run_index_batch(request,Boundary(request))
    assert result['errors']==0 and result['files_parsed']==1
    with sqlite3.connect(request['database']) as conn:assert conn.execute('SELECT status FROM datasets').fetchone()==('ERROR',)


def test_source_change_before_embedding_preserves_index_and_explains_retry(tmp_path):
    request=make_case(tmp_path)
    def change(value):Path(value['path']).write_text('Changed revision',encoding='utf-8')
    io=Boundary(request,convert=change)
    result=native_core.run_index_batch(request,io)
    assert result['errors']==1 and not any(operation==1 for operation,_ in io.events)
    row,=rows(request)
    assert (row['status'],row['error_code'],row['retryable'])==('ERROR','SOURCE_CHANGED_DURING_INDEXING',1)
    assert row['retry_after']>time.time() and row['stage']==''


def test_lost_reply_after_commit_is_recovered_as_confirmed_success(tmp_path):
    request=make_case(tmp_path)
    io=Boundary(request)
    def lost(value):
        io.commit(value)
        return {'error':{'code':'INDEX_STORAGE_WRITE_FAILED','message':'reply lost'}}
    io.publish=lost
    result=native_core.run_index_batch(request,io)
    assert result['errors']==0 and result['chunks']==1 and result['remaining_pending']==0
    assert sum(value.get('action')=='recover_file' for operation,value in io.events if operation==2)==2
    assert len(io.saved)==1


def test_success_reply_without_catalog_commit_is_not_accepted(tmp_path):
    request=make_case(tmp_path)
    result=native_core.run_index_batch(request,Boundary(request,publish=lambda value:{'status':'completed','points':len(value['ids'])}))
    assert result['status']=='failed' and result['error']=='INDEX_PUBLICATION_RESULT_INVALID'
    assert rows(request)[0]['status']=='ERROR'
    assert not list(Path(request['spool_directory']).iterdir())


def test_empty_or_extra_prepared_nodes_never_reach_publication(tmp_path):
    request=make_case(tmp_path)
    def extra(value):
        node={'text':'Actual prepared content','doc_id':'node','payload':{'content_hash':'fixture'}}
        Path(value['node_spool']).write_text((json.dumps(node)+'\n')*2,encoding='utf-8')
        return {'count':1,'route':{}}
    io=Boundary(request,convert=extra)
    result=native_core.run_index_batch(request,io)
    assert result['errors']==1
    assert not any(value.get('action')=='publish_file' for operation,value in io.events if operation==2)
    assert not list(Path(request['spool_directory']).iterdir())


def crash_child(request):
    count=0
    def pause(value):
        nonlocal count
        count+=1
        if count==2:
            Path(value['node_spool']).write_text('partial',encoding='utf-8')
            (Path(request['database']).parent/'paused').write_text('ready')
            time.sleep(60)
    native_core.run_index_batch(request,Boundary(request,convert=pause))


def commit_last_then_pause(request):
    io=Boundary(request)
    def publish(value):
        io.commit(value)
        (Path(request['database']).parent/'last-committed').write_text('ready')
        time.sleep(60)
    io.publish=publish
    native_core.run_index_batch(request,io)


def test_last_file_commit_before_process_death_still_emits_post_index_event(tmp_path):
    request=make_case(tmp_path)
    request_path=tmp_path/'request.json';request_path.write_text(json.dumps(request),encoding='utf-8')
    script=('import json,sys;sys.path[:0]=sys.argv[1:3];from tests.test_light_native_index_batch import commit_last_then_pause;'
        'commit_last_then_pause(json.load(open(sys.argv[3],encoding="utf-8")))')
    root=Path(__file__).resolve().parents[1];program=getattr(sys,'_base_executable',sys.executable)
    with (tmp_path/'child.log').open('wb') as log:
        child=subprocess.Popen([program,'-I','-S','-c',script,str(root),str(root/'.venv/Lib/site-packages'),str(request_path)],stdout=log,stderr=log,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        try:
            deadline=time.monotonic()+15;marker=Path(request['database']).parent/'last-committed'
            while not marker.exists() and time.monotonic()<deadline:
                assert child.poll() is None;time.sleep(.03)
            assert marker.exists()
        finally:
            if child.poll() is None:child.kill()
            child.wait(timeout=10)
    assert rows(request)[0]['status']=='INDEXED'
    io=Boundary(request);result=native_core.run_index_batch(request,io)
    assert result['files_parsed']==0 and result['errors']==0 and result['remaining_pending']==0
    assert not any(operation in (0,1) for operation,_ in io.events)
    with sqlite3.connect(request['database']) as conn:
        assert conn.execute('SELECT dataset_id,kind FROM les_index_events').fetchall()==[('ds','index_completed')]


def test_owner_death_releases_lease_recovers_pending_and_cleans_owned_spools(tmp_path):
    request=make_case(tmp_path,('A.txt','B.txt'))
    request_path=tmp_path/'request.json';request_path.write_text(json.dumps(request),encoding='utf-8')
    script=('import json,sys;sys.path[:0]=sys.argv[1:3];from tests.test_light_native_index_batch import crash_child;'
            'crash_child(json.load(open(sys.argv[3],encoding="utf-8")))')
    root=Path(__file__).resolve().parents[1]
    # Kill the actual interpreter holding the native lease, rather than the
    # Windows venv redirector which would leave its child running.
    program=getattr(sys,'_base_executable',sys.executable)
    with (tmp_path/'child.log').open('wb') as log:
        child=subprocess.Popen([program,'-I','-S','-c',script,str(root),str(root/'.venv/Lib/site-packages'),str(request_path)],stdout=log,stderr=log,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        try:
            deadline=time.monotonic()+15
            marker=Path(request['database']).parent/'paused'
            while not marker.exists() and time.monotonic()<deadline:
                assert child.poll() is None
                time.sleep(.03)
            assert marker.exists()
            # The same OS lease rejects another coordinator without changing dataset status.
            with pytest.raises(native_core.NativeCoreError,match='INDEX_BATCH_BUSY'):
                native_core.run_index_batch(request,Boundary(request))
            child.kill();child.wait(timeout=5)
        finally:
            if child.poll() is None:child.kill();child.wait(timeout=5)
    io=Boundary(request)
    result=native_core.run_index_batch(request,io)
    assert (result['files_parsed'],result['errors'],result['remaining_pending'])==(1,0,0)
    assert len(io.saved)==1
    assert [row['parse_attempts'] for row in rows(request)]==[1,2]
    with sqlite3.connect(request['database']) as conn:
        assert conn.execute("SELECT stop_code FROM les_index_batches WHERE status='interrupted'").fetchone()==('INDEX_BATCH_OWNER_LOST',)
        assert conn.execute("SELECT status FROM datasets").fetchone()==('COMPLETED',)
    assert not list(Path(request['spool_directory']).iterdir())


def test_shipping_ingestion_runs_native_coordinator_pdf_publication_cache_and_page_read(tmp_path,monkeypatch):
    """Production entrypoint + real Rust API/Qdrant/PDF; deterministic vectors test orchestration only."""
    from types import SimpleNamespace
    import httpx
    import pypdfium2_raw
    from qdrant_client import QdrantClient,models
    from backend import native_index_store,qdrant_support as support
    from backend.light_qdrant_runtime import LightQdrantRuntime
    from backend.qdrant_ingestion import QdrantIngestion
    from backend.pdf_reader import read_source_page
    from proxy.services.lexical_index_service import LexicalIndex
    from tests.test_light_native_api import free_port,wait_for
    from tests.pdf_fixtures import write_pdf
    from tests.native_batch_fixtures import NativeBatchEmbeddings
    executable=os.getenv('LES_TEST_NATIVE_API');qdrant=os.getenv('LES_TEST_NATIVE_QDRANT')
    if not executable or not qdrant:pytest.skip('Set native API and Qdrant for complete batch IO acceptance')
    state=tmp_path/'isolated-state';request=make_case(state,('Plain.txt','Drawing.pdf'))
    database=Path(request['database']);pdf=Path(request['content_root'])/'ds/Drawing.pdf'
    write_pdf(pdf,['FIRST-PAGE '+('Alpha project requirement. '*30),'CHECK-PAGE-TWO '+('Second page evidence. '*30)])
    with sqlite3.connect(database) as conn:
        stat=pdf.stat();conn.execute("UPDATE documents SET file_size=?,file_mtime=? WHERE file_name='Drawing.pdf'",(stat.st_size,stat.st_mtime))
    with closing(LexicalIndex(str(database)).connect()):pass
    monkeypatch.setenv('LES_STATE_ROOT',str(state));monkeypatch.setenv('RAG_META_DB_PATH',str(database));monkeypatch.setenv('RAG_LEXICAL_DB_PATH',str(database))
    monkeypatch.setenv('RAG_CHUNK_UNIT','chars');monkeypatch.setenv('RAG_CHUNK_SIZE','1200');monkeypatch.setenv('RAG_CHUNK_OVERLAP','80')
    monkeypatch.setenv('LES_FORMAT_WORKER_MODE','native');monkeypatch.setenv('LES_NATIVE_PDF_WORKER',executable)
    monkeypatch.setenv('LES_NATIVE_PDFIUM',str(Path(pypdfium2_raw.__file__).with_name('pdfium.dll')))
    monkeypatch.setattr(support,'_embedding_cache_descriptor',lambda:{'vector_size':'2','backend':'fixture','model_id':'fixture'})
    monkeypatch.setattr(support,'CHUNK_HASH_CACHE',True)
    embeddings=NativeBatchEmbeddings(database,native=True)
    runtime=LightQdrantRuntime(Path(qdrant),state);api=None;token='2'*64;port=free_port()
    try:
        runtime.start();monkeypatch.setenv('LES_LIGHT_QDRANT_URL',runtime.url)
        environment={**os.environ,'LES_LIGHT_QDRANT_API_KEY':runtime.api_key,'LES_INTERNAL_INGEST_TOKEN':token,'LES_STARTUP_BACKGROUND_MUTATIONS':'false'}
        with (tmp_path/'batch-api.log').open('wb') as log:
            api=subprocess.Popen([executable,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{free_port()}',
                '--state',str(state),'--database',str(database)],env=environment,stdout=log,stderr=log,
                creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        endpoint=f'http://127.0.0.1:{port}/api/light/internal-index-store'
        monkeypatch.setattr(native_index_store,'_bridge',(endpoint,token))
        with httpx.Client(timeout=5,trust_env=False) as client:
            wait_for(lambda:client.post(endpoint).status_code==404)
        with closing(QdrantClient(url=runtime.url,api_key=runtime.api_key,check_compatibility=False)) as vectors:
            vectors.create_collection('docs',vectors_config={'dense':models.VectorParams(size=2,distance=models.Distance.COSINE)},
                sparse_vectors_config={'bm25_sparse':models.SparseVectorParams()})
            calls=embeddings.calls
            adapter=SimpleNamespace(db=MetaDB(str(database)),content_dir=Path(request['content_root']),collection_name='docs',
                qdrant_url=runtime.url,embed=embeddings.embedder)
            monkeypatch.setattr(native_core,'run_index_batch',lambda *_:pytest.fail('Python callback coordinator called'))
            monkeypatch.setattr(QdrantIngestion,'_convert_file',lambda *_:pytest.fail('Python document dispatcher called'))
            result=QdrantIngestion._sync_parse(adapter,'ds',100)
            assert result['owner']=='rust.index-batch' and result['errors']==0,result
            assert result['chunks']>=2 and result['remaining_pending']==0 and len(calls)==result['embedded_chunks']
            assert any(r.get('input')==[] and r.get('keep_alive')==0 for r in embeddings.requests)
            assert all(row['status']=='INDEXED' for row in rows(request))
            stored,_=vectors.scroll('docs',limit=100,with_payload=True,with_vectors=False)
            assert len(stored)==result['chunks']
            pdf_points=[point for point in stored if point.payload.get('file_name')=='Drawing.pdf']
            assert {point.payload.get('page') for point in pdf_points}>={1,2}
            digest=hashlib.sha256(pdf.read_bytes()).hexdigest()
            assert all(point.payload['source_sha256']==digest for point in pdf_points)
            doc_id=next(row['id'] for row in rows(request) if row['file_name']=='Drawing.pdf')
            physical=read_source_page(str(database),{'doc_id':doc_id,'page':2,'source_sha256':digest,'max_chars':12000})
            assert physical['status']=='completed' and 'CHECK-PAGE-TWO' in physical['value']['text']
            assert physical['value']['page']==2 and physical['value']['doc_id']==doc_id
            with sqlite3.connect(database) as conn:conn.execute("UPDATE documents SET status='PENDING'")
            calls.clear()
            repeated=QdrantIngestion._sync_parse(adapter,'ds',100)
            assert repeated['errors']==0 and repeated['embedding_cache_hits']==result['chunks'] and not calls,repeated
            assert vectors.count('docs',exact=True).count==result['chunks']
    finally:
        native_core.required('worker_stop')
        if api is not None:
            api.terminate()
            try:api.wait(timeout=8)
            except subprocess.TimeoutExpired:api.kill();api.wait(timeout=5)
        runtime.stop()
        embeddings.close()
