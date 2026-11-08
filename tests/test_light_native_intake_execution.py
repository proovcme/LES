"""Cold folder intake, native configuration and crash resume without the application server."""
from contextlib import closing
from threading import Event
from pathlib import Path
import hashlib
import json
import os
import sqlite3
import subprocess

import httpx
import pytest
from qdrant_client import QdrantClient, models

from backend import native_core
from backend.document_catalog import MetaDB
from backend.light_qdrant_runtime import LightQdrantRuntime
from backend.workspace_native import catalog_call
from proxy.services.lexical_index_service import LexicalIndex
from tests.native_batch_fixtures import NativeBatchEmbeddings
from tests.pdf_fixtures import write_pdf
from tests.test_light_native_api import free_port, wait_for
from tests.test_light_native_batch_api import stop


def signed_contract(*, model='fixture', backend='openai', size=800, overlap=80, unit='chars'):
    descriptor={'backend':backend,'model_id':model,'profile':'legacy','vector_size':'2'}
    fingerprint=hashlib.sha1('\n'.join(f'{key}={descriptor[key]}' for key in sorted(descriptor)).encode()).hexdigest()
    value={'schema':'les.rag.index-contract.v3','collection':'docs','embedding_model':model,
        'embedding_api_model':model,'embedding_backend':backend,'vector_size':2,'document_embedding_mode':'raw-v1',
        'chunk_unit':unit,'chunk_size':size,'chunk_overlap':overlap,'chunker_revision':'structure-aware-final-budget-v2',
        'qdrant_schema':'named','dense_vector_name':'dense','sparse_vector_name':'bm25_sparse',
        'sparse_tokenizer_revision':'les-bm25-v1','point_embedding_fingerprint':fingerprint,
        'hierarchy_schema':'les.rag.hierarchy.v1','hierarchy_builder':'deterministic-heading-stack-v1',
        'hierarchy_retrieval':'soft-global-plus-descendant-rrf-v1','navigation_evidence_policy':'navigation_not_evidence'}
    return sign(value)


def sign(value):
    value={key:item for key,item in value.items() if key!='fingerprint'}
    value['fingerprint']=hashlib.sha256(json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()).hexdigest()
    return value


def settings():
    return {'EMBEDDING_MODEL':'fixture','EMBED_BACKEND':'openai','RAG_VECTOR_SIZE':'2','RAG_COLLECTION_NAME':'docs',
        'RAG_CHUNK_UNIT':'chars','RAG_CHUNK_SIZE':'800','RAG_CHUNK_OVERLAP':'80','RAG_MIN_CHUNK_CHARS':'20','RAG_CHUNK_HASH_CACHE':'false'}


@pytest.mark.parametrize('change',[
    {'RAG_VECTOR_SIZE':'3'}, {'RAG_COLLECTION_NAME':'other'}, {'EMBEDDING_MODEL':'different'},
    {'EMBED_BACKEND':'ollama'},{'RAG_CHUNK_SIZE':'801'},{'RAG_CHUNK_OVERLAP':'81'},
    {'RAG_CHUNK_UNIT':'tokens'},{'LES_EMBED_PROFILE':'quality'}, {'RAG_SPARSE_TOKENIZER_REVISION':'changed'},
])
def test_native_configuration_refuses_vector_and_chunk_drift(change):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native core')
    with pytest.raises(native_core.NativeCoreError,match='INDEX_CONTRACT_MISMATCH'):
        native_core.required('index_configuration',request={'contract':signed_contract(),'settings':{**settings(),**change}})


def test_native_configuration_preserves_signed_identity_and_rejects_corruption():
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native core')
    contract=signed_contract()
    value=native_core.required('index_configuration',request={'contract':contract,'settings':settings()})
    assert value['embedding_fingerprint']==contract['point_embedding_fingerprint']
    assert value['node_preparation']=={'unit':'chars','size':800,'minimum':20,'window':4,'tokenizer':None}
    assert value['chunking']=={'unit':'chars','chunk_size':800,'chunk_overlap':80}
    contract['chunk_size']=801
    with pytest.raises(native_core.NativeCoreError,match='INDEX_CONTRACT_MISMATCH'):
        native_core.required('index_configuration',request={'contract':contract,'settings':settings()})


@pytest.mark.parametrize('profile',['legacy','quality','qwen','fast'])
def test_native_configuration_matches_existing_profile_contract(tmp_path,monkeypatch,profile):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native core')
    from backend import rag_config
    for key in list(os.environ):
        if key.startswith(('RAG_','EMBED','COREML_EMBED_')) or key in ['LES_EMBED_PROFILE','BGE_MODEL']:
            monkeypatch.delenv(key)
    monkeypatch.setenv('LES_EMBED_PROFILE',profile)
    monkeypatch.setenv('RAG_CHUNK_UNIT','chars')
    monkeypatch.setenv('RAG_INDEX_CONTRACT_PATH',str(tmp_path/'missing.json'))
    contract=rag_config.index_contract_payload()
    value=native_core.required('index_configuration',request={'contract':contract,'settings':{'LES_EMBED_PROFILE':profile,'RAG_CHUNK_UNIT':'chars'}})
    assert value['embedding']==rag_config.point_embedding_descriptor()
    assert value['embedding_fingerprint']==rag_config.point_embedding_fingerprint()
    assert value['chunking']=={key:item for key,item in rag_config.chunking_config().items() if key!='len_fn'}


def test_native_configuration_pins_real_tokenizer_and_rejects_modified_asset(tmp_path):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native core')
    path=tmp_path/'tokenizer.json'
    path.write_text(json.dumps({'version':'1.0','truncation':None,'padding':None,'added_tokens':[],
        'normalizer':None,'pre_tokenizer':{'type':'Whitespace'},'post_processor':None,'decoder':None,
        'model':{'type':'WordLevel','vocab':{'[UNK]':0,'word':1},'unk_token':'[UNK]'}}),encoding='utf-8')
    sha=hashlib.sha256(path.read_bytes()).hexdigest()
    contract=sign({**signed_contract(unit='tokens',size=120,overlap=20),'chunk_tokenizer_sha256':sha})
    config={**settings(),'RAG_CHUNK_UNIT':'tokens','RAG_CHUNK_TOKENS':'120','RAG_CHUNK_OVERLAP_TOKENS':'20'}
    request={'contract':contract,'settings':config,'tokenizer':{'path':str(path)}}
    value=native_core.required('index_configuration',request=request)
    assert value['node_preparation']['tokenizer']=={'path':str(path),'sha256':sha}
    path.write_text(path.read_text().replace('word','changed'),encoding='utf-8')
    with pytest.raises(native_core.NativeCoreError,match='DOCUMENT_TOKENIZER_CHANGED'):
        native_core.required('index_configuration',request=request)


def test_intake_retries_only_due_failures_in_its_own_manifest(tmp_path):
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native core')
    database=tmp_path/'meta.db';MetaDB(str(database))
    with sqlite3.connect(database) as db:db.execute("INSERT INTO datasets(id,name) VALUES('ds','Retry scope')")
    job=catalog_call(database,'intake_enqueue',root='source',dataset_id='ds',parse_limit=5)['id']
    catalog_call(database,'intake_claim')
    files=[{'file_name':name,'source_path':name} for name in ['due.txt','later.txt','permanent.txt','rebound.txt']]
    catalog_call(database,'intake_manifest',id=job,root='source',files=files)
    batch=catalog_call(database,'intake_batch',id=job)
    catalog_call(database,'intake_register',id=job,files=[{**file,'file_size':10,'file_mtime':1.} for file in batch])
    with sqlite3.connect(database) as db:
        db.execute("UPDATE documents SET status='ERROR',retryable=1,retry_after=0,parse_attempts=1,error_code='WORKER_TIMEOUT',last_error='Cause remains visible'")
        db.execute("UPDATE documents SET retry_after=9999999999 WHERE file_name='later.txt'")
        db.execute("UPDATE documents SET retryable=0 WHERE file_name='permanent.txt'")
        db.execute("UPDATE documents SET source_path='new source' WHERE file_name='rebound.txt'")
        db.execute("INSERT INTO documents(id,dataset_id,file_name,status,source_path,retryable,retry_after) VALUES('unowned','ds','other.txt','ERROR','other.txt',1,0)")
    assert catalog_call(database,'intake_retry_due',id=job)=={'retried':1}
    assert catalog_call(database,'intake_index_batch',id=job)==[{'file_name':'due.txt','source_path':'due.txt'}]
    with sqlite3.connect(database) as db:
        assert db.execute("SELECT parse_attempts,last_error FROM documents WHERE file_name='due.txt'").fetchone()==(1,'Cause remains visible')
        assert db.execute("SELECT count(*) FROM documents WHERE status='ERROR'").fetchone()==(4,)


@pytest.mark.parametrize('storage',['server','edge'])
def test_cold_native_intake_model_guard_pdf_and_owner_death(tmp_path,storage):
    program=os.getenv('LES_TEST_NATIVE_API');qdrant=os.getenv('LES_TEST_NATIVE_QDRANT')
    if not program or not qdrant or not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Build native intake assets')
    if storage=='edge' and not os.getenv('LES_TEST_EDGE_VECTOR'):pytest.skip('Build native vector importer')
    import pypdfium2_raw
    state=tmp_path/'state';database=state/'data/meta.db';database.parent.mkdir(parents=True)
    MetaDB(str(database))
    with sqlite3.connect(database) as db:db.execute("INSERT INTO datasets(id,name) VALUES('ds','Native folder')")
    with closing(LexicalIndex(str(database)).connect()):pass
    folder=tmp_path/'Документы с пробелами 東京';folder.mkdir()
    text=folder/'Общее описание.txt';text.write_text('Project source evidence. '*40,encoding='utf-8')
    pdf=write_pdf(folder/'Две страницы.pdf',['First physical page with source evidence. '*20,'Second physical page with independent evidence. '*20])
    baseline={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in folder.iterdir()}
    (state/'.env').write_text('\n'.join(f"{key}='{value}'" for key,value in settings().items())+
        f'\nLES_EXTERNAL_ALLOW_ANY=false\nLES_EXTERNAL_SOURCE_ROOTS="{folder}"\nRAG_UPLOAD_SUFFIXES=.TXT,.PDF\n',encoding='utf-8')
    database.with_name(database.name+'.docs.index-contract.json').write_text(json.dumps(signed_contract()),encoding='utf-8')
    auth=tmp_path/'policy.json';auth.write_text(json.dumps({'schema':'les.native-auth-policy.v1','compatible':True,
        'trusted_role':'admin','proxy_header':'x-les-role','cors_origins':[],'cors_loopback':True}),encoding='utf-8')
    embeddings=NativeBatchEmbeddings(database);api=None
    revision=embeddings.embedder._resolve_embedding_connection().revision_id
    with sqlite3.connect(database) as db:db.execute("DELETE FROM les_model_role_bindings WHERE role='embeddings'")
    runtime=LightQdrantRuntime(Path(qdrant),state);port=free_port();upstream=free_port();token='8'*64;edge=tmp_path/'edge'
    if storage=='edge':
        from tests.test_light_edge_migration import source as edge_source, complete
        data,manifest,rows=edge_source(tmp_path,count=1)
        rows[0]['vector']['dense']=[1.,0.];data.write_text(json.dumps(rows[0])+'\n',encoding='utf-8')
        value=json.loads(manifest.read_text());value.update(collection='docs',source_sha256=hashlib.sha256(data.read_bytes()).hexdigest())
        value['vectors']['dense']['size']=2;manifest.write_text(json.dumps(value),encoding='utf-8')
        code,_,err=complete(Path(os.environ['LES_TEST_EDGE_VECTOR']),'import',manifest,edge);assert code==0,err
    def launch():
        # Explicit clean process configuration: stale test-runner settings cannot override owned .env.
        env={k:v for k,v in os.environ.items() if not (k.startswith(('RAG_','EMBED','COREML_EMBED_','LES_EXTERNAL_','HF_')) or k in ['LES_ENV_PATH','LES_EMBED_PROFILE','BGE_MODEL','LES_NATIVE_EMBEDDING_SPEC'])}
        env.update(LES_LIGHT_QDRANT_URL=runtime.url if storage=='server' else f'http://127.0.0.1:{port}',
            LES_LIGHT_QDRANT_API_KEY=runtime.api_key if storage=='server' else 'intake-edge-key',
            LES_INTERNAL_INGEST_TOKEN=token,LES_STARTUP_BACKGROUND_MUTATIONS='true',
            LES_NATIVE_PDFIUM=str(Path(pypdfium2_raw.__file__).with_name('pdfium.dll')),
            PYTHONHOME=str(tmp_path/'no-python'),PYTHONPATH=str(tmp_path/'no-app'),PATH=str(tmp_path/'no-programs'))
        args=[program,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{upstream}',
            '--state',str(state),'--database',str(database),'--auth-policy-file',str(auth)]
        if storage=='edge':args+=['--edge-storage',str(edge)]
        with (tmp_path/'api.log').open('ab') as log:
            process=subprocess.Popen(args,env=env,stdout=log,stderr=log,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        wait_for(lambda:httpx.post(f'http://127.0.0.1:{port}/api/light/internal-index-batch',trust_env=False).status_code==404)
        return process
    def progress(id):return catalog_call(database,'intake_get',id=id)
    try:
        if storage=='server':runtime.start()
        url=runtime.url if storage=='server' else f'http://127.0.0.1:{port}'
        key=runtime.api_key if storage=='server' else 'intake-edge-key'
        if storage=='server':
            with closing(QdrantClient(url=url,api_key=key,check_compatibility=False)) as vectors:
                vectors.create_collection('docs',vectors_config={'dense':models.VectorParams(size=2,distance=models.Distance.COSINE)},sparse_vectors_config={'bm25_sparse':models.SparseVectorParams()})
        api=launch()
        with httpx.Client(base_url=f'http://127.0.0.1:{port}',trust_env=False,timeout=30) as client:
            outside=client.post('/api/rag/index-external',json={'path':str(tmp_path),'dataset_id':'ds','background':True,'parse':False}).raise_for_status().json()['intake_job_id']
            wait_for(lambda:progress(outside)['reason_code']=='INTAKE_OUTSIDE_ALLOWED_ROOT')
            assert not embeddings.calls
            response=client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':'ds','background':True,'parse_limit':1}).raise_for_status()
            assert response.headers['x-les-handler']=='rust.intake';job=response.json()['intake_job_id']
            wait_for(lambda:progress(job)['reason_code']=='EMBEDDING_MODEL_NOT_ASSIGNED')
            assert not embeddings.calls and progress(job)['processed']==2
            from proxy.services.model_connection_contracts import ConnectionRole
            from proxy.services.model_connection_registry_service import ModelConnectionRegistry
            ModelConnectionRegistry(str(database)).bind_role(ConnectionRole.EMBEDDINGS,
                revision,expected_binding_revision=None,actor='admin:test')
            with sqlite3.connect(database) as db:db.execute('UPDATE native_intakes SET retry_at=0 WHERE id=?',[job])
            wait_for(lambda:progress(job)['status']=='completed',timeout=40)
            with closing(QdrantClient(url=url,api_key=key,check_compatibility=False)) as vectors:
                points,_=vectors.scroll('docs',limit=100,with_payload=True)
            pdf_points=[p.payload for p in points if p.payload.get('dataset_id')=='ds' and p.payload.get('file_name','').endswith('.pdf')]
            assert {p['page'] for p in pdf_points if p.get('page') is not None}=={1,2}
            assert {'#page=1','#page=2'} <= {p.get('source_ref','')[p.get('source_ref','').find('#page='):] for p in pdf_points}
            assert {p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in folder.iterdir()}==baseline
            text.write_text('Updated source revision with different content. '*35,encoding='utf-8')
            arrived=Event();release=Event()
            def pause(value):
                arrived.set();release.wait(30)
            embeddings.on_request=pause
            again=client.post('/api/rag/index-external',json={'path':str(folder),'dataset_id':'ds','background':True,'parse_limit':1}).raise_for_status().json()['intake_job_id']
            assert arrived.wait(20);lost_pid=api.pid;stop(api);embeddings.on_request=None;release.set()
            api=launch();wait_for(lambda:progress(again)['status']=='completed',timeout=40)
            assert progress(again)['recoveries']>=1
            with sqlite3.connect(database) as db:
                assert db.execute("SELECT owner_pid,stop_code FROM les_index_batches WHERE status='interrupted'").fetchone()==(lost_pid,'INDEX_BATCH_OWNER_LOST')
                assert db.execute("SELECT count(*) FROM documents WHERE status='INDEXED' AND chunk_count>0").fetchone()==(2,)
            assert not list((state/'data/index-publish').iterdir())
    finally:
        embeddings.on_request=None
        if 'release' in locals():release.set()
        stop(api)
        if storage=='server':runtime.stop()
        embeddings.close()
