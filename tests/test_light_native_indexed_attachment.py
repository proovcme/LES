"""Public indexed upload must survive the loss of its only Rust worker."""
from contextlib import closing
import hashlib
import os
from pathlib import Path
import sqlite3
import subprocess
from threading import Event

import httpx
import pytest
from qdrant_client import QdrantClient

from tests.native_batch_fixtures import NativeBatchEmbeddings
from tests.pdf_fixtures import write_pdf
from tests.test_light_native_api import free_port, wait_for
from tests.test_light_native_intake_execution import settings


def exercise(tmp_path, files):
    import pypdfium2_raw
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program or not os.getenv('LES_TEST_EDGE_API'):
        pytest.skip('Build the Edge-enabled API')
    state=tmp_path/'state';database=state/'data/meta.db';database.parent.mkdir(parents=True)
    hashes={path.name:hashlib.sha256(path.read_bytes()).hexdigest() for path in files}
    (state/'.env').write_text('\n'.join(f"{key}='{value}'" for key,value in settings().items())+
        '\nLES_EXTERNAL_ALLOW_ANY=false\nRAG_UPLOAD_SUFFIXES=.PDF,.XLS,.TXT\n',encoding='utf-8')
    embeddings=None
    port=free_port();url=f'http://127.0.0.1:{port}';key='fixture-edge-upload-key'
    env={k:v for k,v in os.environ.items() if not (k.startswith(('RAG_','EMBED','COREML_EMBED_','LES_EXTERNAL_','HF_')) or
        k in ['LES_ENV_PATH','LES_EMBED_PROFILE','BGE_MODEL','LES_NATIVE_EMBEDDING_SPEC'])}
    env.update(LES_LIGHT_QDRANT_URL=url,LES_LIGHT_QDRANT_API_KEY=key,
        LES_INTERNAL_INGEST_TOKEN='9'*64,LES_STARTUP_BACKGROUND_MUTATIONS='true',
        LES_NATIVE_PDFIUM=str(Path(pypdfium2_raw.__file__).with_name('pdfium.dll')),
        PYTHONHOME=str(tmp_path/'no-python'),PYTHONPATH=str(tmp_path/'no-app'),PATH=str(tmp_path/'no-programs'))
    args=[program,'--listen',f'127.0.0.1:{port}','--state',str(state),'--database',str(database),
        '--chat-owner','rust','--edge-storage',str(tmp_path/'edge')]
    api=None
    arrived=Event();release=Event()
    endpoint='/api/rag/attach?mode=index'
    with httpx.Client(base_url=url,trust_env=False,timeout=30) as client:
        def start():
            nonlocal api
            with (tmp_path/'api.log').open('ab') as log:
                api=subprocess.Popen(args,env=env,stdout=log,stderr=log,creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
            wait_for(lambda:client.get('/api/documents/datasets').is_success,timeout=20)
        def progress(receipt):
            return client.get('/api/light/intake',params={'id':receipt['intake_job_id']}).raise_for_status().json()
        def upload(path,index):
            with path.open('rb') as file:
                response=client.post(endpoint,files={'file':(path.name,file)},headers={'Idempotency-Key':f'fixture-index-{index}'})
            assert response.headers['x-les-handler']=='rust.attachments'
            return response.raise_for_status().json()
        try:
            start()
            receipts=[upload(path,i) for i,path in enumerate(files)]
            assert len({row['dataset_id'] for row in receipts})==1
            assert all(row['status']=='queued' and row['mode']=='index' for row in receipts)
            assert [upload(path,i) for i,path in enumerate(files)]==receipts
            assert client.post(endpoint,files={'file':(files[0].name,b'new bytes')},
                headers={'Idempotency-Key':'fixture-index-0'}).status_code==409
            for receipt in receipts:
                wait_for(lambda:progress(receipt)['reason_code']=='EMBEDDING_MODEL_NOT_ASSIGNED',timeout=15)
            with closing(sqlite3.connect(database)) as db:
                assert db.execute("SELECT count(*) FROM documents").fetchone()[0]==len(files)
                assert db.execute("SELECT sensitivity FROM datasets").fetchone()==('P0',)
            def pause(value):
                if value['input']!=['LES embedding dimension verification']:
                    arrived.set();release.wait(30)
            embeddings=NativeBatchEmbeddings(database)
            embeddings.on_request=pause
            with closing(sqlite3.connect(database)) as db, db:
                db.execute("UPDATE native_intakes SET retry_at=0")
            assert arrived.wait(25), (tmp_path/'api.log').read_text(encoding='utf-8',errors='replace')
            api.kill();api.wait(timeout=10)
            embeddings.on_request=None;release.set()
            start()
            for receipt in receipts:
                wait_for(lambda:progress(receipt)['status']=='completed',timeout=50)
            assert sum(progress(row)['recoveries'] for row in receipts)>=1
            assert all(progress(row)['indexed']==1 for row in receipts)
            with closing(QdrantClient(url=url,api_key=key,check_compatibility=False)) as vectors:
                points,_=vectors.scroll('docs',limit=1000,with_payload=True)
            pdf=next(row for row in receipts if row['name'].lower().endswith('.pdf'))
            with closing(sqlite3.connect(database)) as db:
                indexed_name=db.execute('SELECT file_name FROM documents WHERE id=?',[pdf['doc_id']]).fetchone()[0]
            assert any(p.payload.get('page')==2 and p.payload.get('source_ref','').endswith('#page=2')
                for p in points if p.payload.get('file_name')==indexed_name and p.payload.get('dataset_id')==pdf['dataset_id'])
            for receipt in receipts:
                raw=client.get(f"/api/documents/by-id/{receipt['doc_id']}/raw").raise_for_status()
                assert hashlib.sha256(raw.content).hexdigest()==hashes[receipt['name']]
            assert [upload(path,i) for i,path in enumerate(files)]==receipts
            assert {path.name:hashlib.sha256(path.read_bytes()).hexdigest() for path in files}==hashes
            assert not list((state/'storage/upload-tmp').iterdir())
        finally:
            release.set()
            if api is not None and api.poll() is None:
                api.terminate();api.wait(timeout=10)
            if embeddings is not None:embeddings.close()


def test_indexed_attachment_queue_replays_and_resumes(tmp_path):
    sources=tmp_path/'Источники с пробелами';sources.mkdir()
    pdf=write_pdf(sources/'Редакция 東京.pdf',['First physical page and source evidence. '*20,
        'Second physical page and verified citation. '*20])
    note=sources/'Описание.txt';note.write_text('Содержание контрольного документа. '*25,encoding='utf-8')
    exercise(tmp_path,[pdf,note])
