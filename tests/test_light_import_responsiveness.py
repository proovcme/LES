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
