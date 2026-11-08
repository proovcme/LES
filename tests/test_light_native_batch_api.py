"""The entire batch executes in the native service with no live Python application server."""
from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime,timezone
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from threading import Thread,Event
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess

import httpx
import pytest
from qdrant_client import QdrantClient,models
from backend.light_qdrant_runtime import LightQdrantRuntime
from proxy.services.lexical_index_service import LexicalIndex
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_contracts import ConnectionLocality,CapabilitySnapshot,CapabilityObservation,CapabilityName,CapabilityState
from tests.test_light_native_api import free_port,wait_for
from tests.test_light_native_index_batch import make_case
from tests.test_light_native_format_dispatch import source


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:process.wait(timeout=8)
        except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)


@pytest.mark.parametrize('storage',['server','edge'])
def test_native_http_batch_owns_conversion_embedding_publication_and_crash_resume(tmp_path,storage):
    program=os.getenv('LES_TEST_NATIVE_API');qdrant=os.getenv('LES_TEST_NATIVE_QDRANT')
    if not program or not qdrant or not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Native API/core/Qdrant required')
    if storage=='edge' and not os.getenv('LES_TEST_EDGE_VECTOR'):pytest.skip('Native vector importer required')
    state=tmp_path/'state';request=make_case(state,('book.xlsx',));database=Path(request['database'])
    path=Path(request['content_root'])/'ds/book.xlsx';source(path)
    with sqlite3.connect(database) as db:
        info=path.stat();db.execute('UPDATE documents SET file_size=?,file_mtime=?',(info.st_size,info.st_mtime))
    with closing(LexicalIndex(str(database)).connect()):pass
    request.update(chunking={'unit':'chars','chunk_size':800,'chunk_overlap':80},
        node_preparation={'unit':'chars','size':800,'minimum':20,'tokenizer':None,'window':4},
        vector_names={'dense':'dense','sparse':'bm25_sparse'},
        format_worker={'program':'never-run-this-untrusted-program.exe','directory':'invalid',
                       'timeout_ms':30000,'minimum':20,'final_minimum':20,'table_row_limit':600})
    seen=[];pause=Event();arrived=Event();release=Event()
    class Model(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):
            data=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            seen.append(data)
            assert self.path=='/v1/embeddings' and data['model']=='fixture'
            if pause.is_set():arrived.set();release.wait(30)
            body=json.dumps({'model':'fixture','data':[{'index':i,'embedding':[1.,0.]} for i in range(len(data['input']))]}).encode()
            try:
                self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
            except OSError:pass
    model=ThreadingHTTPServer(('127.0.0.1',0),Model);thread=Thread(target=model.serve_forever,daemon=True);thread.start()
    registry=ModelConnectionRegistry(str(database))
    revision=registry.create_connection(display_name='Batch fixture',base_url=f'http://127.0.0.1:{model.server_port}/v1',
        model_id='fixture',locality=ConnectionLocality.LOOPBACK,requested_context_tokens=4096,secret_ref=None,extension_type=None,actor='admin:test')
    observed=datetime(2026,1,1,tzinfo=timezone.utc)
    registry.save_capability_snapshot(CapabilitySnapshot('cap:batch',revision.revision_id,
        (CapabilityObservation(CapabilityName.EMBEDDINGS,CapabilityState.SUPPORTED,'probe',observed),),
        observed,datetime(2099,1,1,tzinfo=timezone.utc),{'observed_context_tokens':'4096'}),actor='admin:test')
    request['embedding_execution']={'revision_id':revision.revision_id,'allow_remote':False}
    runtime=LightQdrantRuntime(Path(qdrant),state);api=None;port=free_port();upstream=free_port();token='9'*64
    edge=tmp_path/'edge'
    if storage=='edge':
        from tests.test_light_edge_migration import source as edge_source,complete
        vector_program=os.getenv('LES_TEST_EDGE_VECTOR')
        import_path,manifest,rows=edge_source(tmp_path,count=1)
        rows[0]['vector']['dense']=[1.,0.]
        import_path.write_text(json.dumps(rows[0],ensure_ascii=False)+'\n',encoding='utf-8')
        value=json.loads(manifest.read_text(encoding='utf-8'));value.update(collection='docs',source_sha256=hashlib.sha256(import_path.read_bytes()).hexdigest())
        value['vectors']['dense']['size']=2;manifest.write_text(json.dumps(value),encoding='utf-8')
        code,_,err=complete(Path(vector_program),'import',manifest,edge);assert code==0,err
        # Originals and immutable derivatives have deliberately different owners.
        request['content_root']=str(edge/'content')
    def launch():
        owned_url=runtime.url if storage=='server' else f'http://127.0.0.1:{port}'
        storage_key=runtime.api_key if storage=='server' else 'edge-batch-fixture'
        env={**os.environ,'LES_LIGHT_QDRANT_URL':owned_url,'LES_LIGHT_QDRANT_API_KEY':storage_key,
             'LES_INTERNAL_INGEST_TOKEN':token,'LES_STARTUP_BACKGROUND_MUTATIONS':'false',
             'PYTHONHOME':str(tmp_path/'no-python'),'PYTHONPATH':str(tmp_path/'no-application'),'PATH':str(tmp_path/'no-programs')}
        env.pop('LES_NATIVE_EMBEDDING_SPEC',None)
        args=[program,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{upstream}','--state',str(state),'--database',str(database)]
        if storage=='edge':args+=['--edge-storage',str(edge)]
        with (tmp_path/'api.log').open('ab') as log:
            process=subprocess.Popen(args,env=env,stdout=log,stderr=log,
                creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        wait_for(lambda:httpx.post(f'http://127.0.0.1:{port}/api/light/internal-index-batch',trust_env=False).status_code==404)
        if storage=='edge':wait_for(lambda:httpx.get(f'http://127.0.0.1:{port}/collections',headers={'api-key':'edge-batch-fixture'},trust_env=False).status_code==200)
        return process
    try:
        if storage=='server':runtime.start()
        api=launch()
        vector_url=runtime.url if storage=='server' else f'http://127.0.0.1:{port}'
        storage_key=runtime.api_key if storage=='server' else 'edge-batch-fixture'
        with httpx.Client(base_url=f'http://127.0.0.1:{port}',trust_env=False,timeout=45) as client,closing(QdrantClient(url=vector_url,api_key=storage_key,check_compatibility=False)) as vectors:
            if storage=='server':vectors.create_collection('docs',vectors_config={'dense':models.VectorParams(size=2,distance=models.Distance.COSINE)},sparse_vectors_config={'bm25_sparse':models.SparseVectorParams()})
            endpoint='/api/light/internal-index-batch';headers={'x-les-index-token':token}
            assert client.post(endpoint,json=request).status_code==404
            assert client.post(endpoint,json=request,headers={**headers,'Origin':str(client.base_url)}).status_code==404
            assert client.post(endpoint,json={**request,'database':str(tmp_path/'outside.db')},headers=headers).status_code==409
            unconfigured={**request};unconfigured.pop('embedding_execution')
            denied=client.post(endpoint,json=unconfigured,headers=headers)
            assert denied.status_code==409 and denied.json()['detail']=='EMBEDDING_NATIVE_EXECUTION_REQUIRED' and not seen
            unsupported=state/'storage/datasets/ds/mail.eml';unsupported.write_text('From: test@example.invalid\n\nNative admission fixture',encoding='utf-8')
            with sqlite3.connect(database) as db:
                db.execute("INSERT INTO documents(id,dataset_id,file_name,status,source_path) VALUES('unsupported','ds','mail.eml','PENDING','')")
            denied=client.post(endpoint,json=request,headers=headers)
            assert denied.status_code==412 and denied.json()['detail']=='DOCUMENT_NATIVE_DISPATCH_REQUIRED' and not seen
            with sqlite3.connect(database) as db:
                assert db.execute("SELECT count(*) FROM documents WHERE status!='PENDING'").fetchone()==(0,)
                db.execute("DELETE FROM documents WHERE id='unsupported'")
            unsupported.unlink()
            mismatch={**request,'embedding':{**request['embedding'],'model_id':'different-model'}}
            denied=client.post(endpoint,json=mismatch,headers=headers)
            assert denied.status_code==409 and denied.json()['detail']=='EMBEDDING_INDEX_MODEL_MISMATCH' and not seen
            result=client.post(endpoint,json=request,headers=headers).raise_for_status()
            assert result.headers['x-les-handler']=='rust.index-batch'
            value=result.json();assert value['files_parsed']==1 and value['errors']==0,value
            before,_=vectors.scroll('docs',limit=100,with_payload=True,with_vectors=True)
            frozen={p.id:(p.payload,p.vector) for p in before};assert frozen and seen
            old_ref=next(p.payload['parquet_path'] for p in before if p.payload.get('dataset_id')=='ds');old_blob=Path(request['content_root'])/'ds'/old_ref
            old_bytes=old_blob.read_bytes()
            source(path,7)
            with sqlite3.connect(database) as db:
                info=path.stat();db.execute("UPDATE documents SET status='PENDING',file_size=?,file_mtime=?",(info.st_size,info.st_mtime))
            originals=hashlib.sha256(path.read_bytes()).hexdigest();pause.set()
            with ThreadPoolExecutor(max_workers=1) as executor:
                future=executor.submit(client.post,endpoint,json=request,headers=headers)
                assert arrived.wait(15)
                # A second owner cannot enter this batch; reads and old blobs remain valid.
                busy=client.post(endpoint,json=request,headers=headers);assert busy.status_code==409 and busy.json()['detail']=='INDEX_BATCH_BUSY'
                during,_=vectors.scroll('docs',limit=100,with_payload=True,with_vectors=True)
                assert {p.id:(p.payload,p.vector) for p in during}==frozen
                assert old_blob.read_bytes()==old_bytes
                stopped_pid=api.pid;stop(api)
                with pytest.raises(httpx.HTTPError):future.result()
                release.set();pause.clear()
            api=launch()
            value=client.post(endpoint,json=request,headers=headers).raise_for_status().json()
            assert value['files_parsed']==1 and value['errors']==0,value
            with sqlite3.connect(database) as db:
                assert db.execute("SELECT owner_pid,stop_code FROM les_index_batches WHERE status='interrupted'").fetchone()==(stopped_pid,'INDEX_BATCH_OWNER_LOST')
                assert db.execute('SELECT status FROM documents').fetchone()==('INDEXED',)
            after,_=vectors.scroll('docs',limit=100,with_payload=True,with_vectors=True)
            assert after and {p.payload['parquet_path'] for p in after if p.payload.get('dataset_id')=='ds'}!={old_ref}
            assert old_blob.read_bytes()==old_bytes and hashlib.sha256(path.read_bytes()).hexdigest()==originals
            assert not list(Path(request['spool_directory']).iterdir())
            # Losing only the caller must not stop the native durable owner.
            source(path,9)
            with sqlite3.connect(database) as db:
                info=path.stat();db.execute("UPDATE documents SET status='PENDING',file_size=?,file_mtime=?",(info.st_size,info.st_mtime))
            arrived.clear();release.clear();pause.set()
            with httpx.Client(base_url=str(client.base_url),trust_env=False,timeout=.2) as departing:
                with pytest.raises(httpx.ReadTimeout):departing.post(endpoint,json=request,headers=headers)
            assert arrived.wait(5);assert api.poll() is None
            release.set();pause.clear()
            def completed_without_caller():
                with sqlite3.connect(database) as db:
                    return (db.execute('SELECT status FROM documents').fetchone()==('INDEXED',)
                        and db.execute("SELECT count(*) FROM les_index_batches WHERE status='completed'").fetchone()==(3,))
            wait_for(completed_without_caller)
            with sqlite3.connect(database) as db:
                assert db.execute("SELECT count(*) FROM les_index_batches WHERE status='completed'").fetchone()==(3,)
            assert not list(Path(request['spool_directory']).iterdir())
    finally:
        release.set();stop(api)
        if storage=='server':runtime.stop()
        model.shutdown();model.server_close();thread.join(timeout=5)
