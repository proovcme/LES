"""Registration must neither parse sources nor block health/other API work."""
import asyncio
import threading
from types import SimpleNamespace

import pytest
from backend.document_catalog import MetaDB
from backend.qdrant_adapter import QdrantLlamaIndexAdapter
from backend import qdrant_support


@pytest.mark.asyncio
async def test_slow_catalog_does_not_block_the_event_loop_and_no_parser_is_opened(tmp_path, monkeypatch):
    source = tmp_path / 'Документ 日本語.pdf'
    source.write_bytes(b'%PDF-accepted-for-registration-only')
    entered, release = threading.Event(), threading.Event()
    classification = []
    monkeypatch.setattr(qdrant_support, 'route_document', lambda path: classification.append(path))

    def add(*args, **kwargs):
        entered.set()
        assert release.wait(3)
        return 'document-id', True, True

    backend = SimpleNamespace(db=SimpleNamespace(add_document=add), content_dir=tmp_path/'state')
    task = asyncio.create_task(QdrantLlamaIndexAdapter.register_external_file(backend, 'dataset', source, source.name))
    try:
        assert await asyncio.to_thread(entered.wait, 2)
        assert not task.done()  # The event loop remains available during SQLite IO.
        assert classification == []
    finally:
        release.set()
    assert await task == 'document-id'
    assert source.read_bytes() == b'%PDF-accepted-for-registration-only'


@pytest.mark.asyncio
async def test_repeated_registration_preserves_claimed_status_and_old_index_until_replacement(tmp_path):
    source = tmp_path / 'Документ.txt'; source.write_text('source',encoding='utf-8')
    catalog = MetaDB(str(tmp_path/'catalog.db'))
    with catalog._get_conn() as connection:
        connection.execute("INSERT INTO datasets(id,name) VALUES('ds','Dataset')")
    backend = SimpleNamespace(db=catalog,content_dir=tmp_path/'derived')
    async def register(**kwargs):
        return await QdrantLlamaIndexAdapter.register_external_file(backend,'ds',source,source.name,**kwargs)
    identifier = await register()
    catalog.update_document_status('ds',source.name,'INDEXED',7)
    assert await register() == identifier
    with catalog._get_conn() as connection:
        assert tuple(connection.execute('SELECT status,chunk_count FROM documents WHERE id=?',(identifier,)).fetchone()) == ('INDEXED',7)
    assert await register(force_reindex=True) == identifier
    with catalog._get_conn() as connection:
        assert tuple(connection.execute('SELECT status,chunk_count FROM documents WHERE id=?',(identifier,)).fetchone()) == ('PENDING',7)


@pytest.mark.asyncio
async def test_missing_external_source_is_not_registered_as_an_empty_file(tmp_path):
    backend=SimpleNamespace(db=SimpleNamespace(add_document=lambda *a,**k: pytest.fail('missing source registered')),content_dir=tmp_path/'derived')
    with pytest.raises(FileNotFoundError):
        await QdrantLlamaIndexAdapter.register_external_file(backend,'ds',tmp_path/'missing.pdf','missing.pdf')


@pytest.mark.asyncio
@pytest.mark.parametrize('scan_fails',[False,True])
async def test_background_intake_keeps_health_responsive_and_records_actual_result(tmp_path,monkeypatch,scan_fails):
    import httpx
    from fastapi import FastAPI
    from proxy.routers import dataset_external as external
    from proxy.services.job_service import JobService
    from proxy.services import dataset_runtime
    root=tmp_path/'Источники'; root.mkdir()
    for number in range(5):
        (root/f'Документ {number}.txt').write_text('Не менять оригинал 🌲',encoding='utf-8')
    originals={path:path.read_bytes() for path in root.iterdir()}
    catalog=MetaDB(str(tmp_path/'catalog.db'))
    jobs=JobService(tmp_path/'catalog.db')
    with catalog._get_conn() as connection:
        connection.execute("INSERT INTO datasets(id,name) VALUES('ds','Documents')")
    backend=SimpleNamespace(db=catalog,content_dir=tmp_path/'derived')
    async def datasets(): return [SimpleNamespace(id='ds',name='Documents')]
    async def register(dataset_id,path,name):
        return await QdrantLlamaIndexAdapter.register_external_file(backend,dataset_id,path,name)
    backend.list_datasets=datasets; backend.register_external_file=register
    state=SimpleNamespace(backend=backend,job_service=jobs)
    monkeypatch.setattr(dataset_runtime,'get_dataset_state',lambda:state)
    monkeypatch.setattr(external,'validate_external_source',lambda value:root)
    entered,release=threading.Event(),threading.Event()
    original_scan=external._external_supported_files
    def scan(path):
        entered.set()
        assert release.wait(3)
        if scan_fails: raise PermissionError('fixture unreadable directory')
        return original_scan(path)
    monkeypatch.setattr(external,'_external_supported_files',scan)
    application=FastAPI(); application.include_router(external.router)
    application.dependency_overrides[external.require_admin]=lambda:object()
    @application.get('/health')
    async def health(): return {'ready':True}
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=application),base_url='http://test') as client:
        response=await client.post('/api/rag/index-external',json={'path':str(root),'dataset_id':'ds','parse':False,'background':True})
        job_id=response.json()['intake_job_id']
        try:
            assert await asyncio.to_thread(entered.wait,2)
            assert jobs.get(job_id)['status'].lower()=='running'
            assert (await client.get('/health')).json()=={'ready':True}
        finally:
            release.set()
        for _ in range(200):
            job=await asyncio.to_thread(jobs.get,job_id)
            if job['status'].lower() in {'completed','failed'}: break
            await asyncio.sleep(.01)
    assert job['status'].lower()==('failed' if scan_fails else 'completed')
    assert job['errors']==int(scan_fails)
    if not scan_fails: assert job['processed']==job['total']==5
    assert {path:path.read_bytes() for path in root.iterdir()}==originals
