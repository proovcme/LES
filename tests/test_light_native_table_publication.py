"""Real native writer: table blobs follow the committed document generation."""
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import subprocess
from types import SimpleNamespace

import httpx
import pytest
from qdrant_client import QdrantClient,models
from backend import native_core,native_index_store,qdrant_support as support
from backend.light_qdrant_runtime import LightQdrantRuntime
from backend.native_parquet import read_rows,write
from backend.qdrant_ingestion import QdrantIngestion
from backend.document_catalog import MetaDB
from proxy.services.lexical_index_service import LexicalIndex
from tests.test_light_native_api import free_port,wait_for
from tests.test_light_native_index_batch import make_case
from tests.test_light_native_format_dispatch import source,dispatch
from tests.native_batch_fixtures import NativeBatchEmbeddings


def test_table_artifacts_publish_with_index_and_old_blob_survives_revision(tmp_path,monkeypatch):
    api_program=os.getenv('LES_TEST_NATIVE_API');qdrant=os.getenv('LES_TEST_NATIVE_QDRANT')
    if not api_program or not qdrant or not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Native API/core/Qdrant required')
    state=tmp_path/'state';request=make_case(state,('book.xlsx',));database=Path(request['database'])
    content=Path(request['content_root']);path=content/'ds/book.xlsx';source(path)
    def pending():
        info=path.stat()
        with sqlite3.connect(database) as db:db.execute("UPDATE documents SET status='PENDING',file_size=?,file_mtime=? WHERE id='doc-0'",(info.st_size,info.st_mtime))
    pending()
    with closing(LexicalIndex(str(database)).connect()):pass
    for key,value in {'LES_STATE_ROOT':str(state),'RAG_META_DB_PATH':str(database),'RAG_LEXICAL_DB_PATH':str(database),
        'LES_NATIVE_FORMAT_WORKER':api_program,'RAG_CHUNK_UNIT':'chars','RAG_CHUNK_SIZE':'800','RAG_CHUNK_OVERLAP':'80'}.items():monkeypatch.setenv(key,value)
    monkeypatch.setattr(support,'_embedding_cache_descriptor',lambda:{'vector_size':'2','backend':'fixture','model_id':'fixture'})
    monkeypatch.setattr(QdrantIngestion,'_convert_file',lambda *_:pytest.fail('Retained Python conversion called'))
    monkeypatch.setattr(support,'route_document',lambda *_:pytest.fail('Retained classifier called'))
    embeddings=NativeBatchEmbeddings(database)
    runtime=LightQdrantRuntime(Path(qdrant),state);api=None;listen=free_port();upstream=free_port();token='9'*64
    try:
        runtime.start();monkeypatch.setenv('LES_LIGHT_QDRANT_URL',runtime.url)
        env={**os.environ,'LES_LIGHT_QDRANT_API_KEY':runtime.api_key,'LES_INTERNAL_INGEST_TOKEN':token,'LES_STARTUP_BACKGROUND_MUTATIONS':'false'}
        with (tmp_path/'api.log').open('wb') as log:
            api=subprocess.Popen([api_program,'--listen',f'127.0.0.1:{listen}','--upstream',f'http://127.0.0.1:{upstream}',
                '--state',str(state),'--database',str(database)],env=env,stdout=log,stderr=log)
        with httpx.Client(base_url=f'http://127.0.0.1:{listen}',trust_env=False,timeout=20) as client,closing(QdrantClient(url=runtime.url,api_key=runtime.api_key,check_compatibility=False)) as vectors:
            wait_for(lambda:client.post('/api/light/internal-index-store').status_code==404)
            vectors.create_collection('docs',vectors_config={'dense':models.VectorParams(size=2,distance=models.Distance.COSINE)},
                sparse_vectors_config={'bm25_sparse':models.SparseVectorParams()})
            monkeypatch.setattr(native_index_store,'_bridge',(f'http://127.0.0.1:{listen}/api/light/internal-index-store',token))
            adapter=SimpleNamespace(db=MetaDB(str(database)),content_dir=content,collection_name='docs',qdrant_url=runtime.url,
                embed=embeddings.embedder)
            result=QdrantIngestion._sync_parse(adapter,'ds',100)
            assert result['files_parsed']==1 and result['errors']==0,result
            def refs():return native_core.required('tables',request={'action':'references','database':str(database),
                'storage_root':str(content),'dataset_ids':['ds']})['refs']
            first=refs();assert len(first)==1 and first[0]['file_name']=='book.xlsx'
            old=first[0];old_path=content/'ds'/old['parquet_path'];old_bytes=old_path.read_bytes()
            assert len(read_rows(old_path,old['parquet_sha256']))==4
            legacy=content/'ds/_parquet/book.parquet';write([{'name':'stale legacy duplicate','qty':999}],legacy)
            assert refs()==first  # Directory scans must not count an older canonical file too.

            source(path,count=5)
            # Prepare a candidate before publication; its version exists but is hidden.
            candidate=dispatch(path,content/'ds',file_key='book.xlsx')
            assert candidate['nodes'][0]['payload']['parquet_path']!=old['parquet_path']
            assert refs()==first and old_path.read_bytes()==old_bytes
            pending()
            observed=[]
            def before_publish(_):
                with sqlite3.connect(database) as db:
                    active=db.execute('SELECT path FROM les_table_artifacts WHERE dataset_id=? AND file_name=?',('ds','book.xlsx')).fetchone()[0]
                assert active==old['parquet_path'] and old_path.read_bytes()==old_bytes
                observed.append(True)
            embeddings.on_request=before_publish
            result=QdrantIngestion._sync_parse(adapter,'ds',100)
            assert result['errors']==0 and observed,result
            new=refs();assert len(new)==1 and new[0]['parquet_path']!=old['parquet_path']
            assert len(read_rows(content/'ds'/new[0]['parquet_path'],new[0]['parquet_sha256']))==6
            assert len(read_rows(old_path,old['parquet_sha256']))==4
            stored,_=vectors.scroll('docs',limit=100,with_payload=True,with_vectors=False)
            assert all(point.payload.get('parquet_path')==new[0]['parquet_path'] for point in stored)
    finally:
        native_core.required('worker_stop')
        if api is not None:
            api.terminate()
            try:api.wait(timeout=8)
            except subprocess.TimeoutExpired:api.kill();api.wait(timeout=5)
        runtime.stop()
        embeddings.close()
