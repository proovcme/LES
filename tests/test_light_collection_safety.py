"""Startup must never erase live points based on advisory collection counters."""
import asyncio
from types import SimpleNamespace

import pytest
from qdrant_client.http.exceptions import UnexpectedResponse

from backend import qdrant_support as support
from backend.index_replacement import ReplacementJournal
from backend.interface import EmbeddingContractError
from backend.qdrant_collection import QdrantCollection

pytestmark = pytest.mark.anyio


@pytest.fixture
def anyio_backend():
    return 'asyncio'


@pytest.fixture
def collection(tmp_path, monkeypatch):
    class Client:
        def __init__(self):
            self.points = ['preserve-this-point']
            self.reported = 0
            self.compatible = False
            self.calls = []
            self.count_failure = None
            self.get_failure = None

        async def get_collection(self, _name):
            self.calls.append('get')
            if self.get_failure:
                raise self.get_failure
            return dict(points_count=self.reported, config=dict(params=dict(
                vectors={'dense': {'size': 3}} if self.compatible else {'size': 3},
                sparse_vectors={'bm25_sparse': {}} if self.compatible else None)))

        async def count(self, *, exact, **kwargs):
            assert exact is True
            self.calls.append('count')
            if self.count_failure:
                raise self.count_failure
            return SimpleNamespace(count=len(self.points))

        async def delete_collection(self, _name):
            self.calls.append('delete')
            self.points.clear()

        async def create_collection(self, **_kwargs):
            self.calls.append('create')
            self.compatible = True
            self.get_failure = None

    adapter = QdrantCollection()
    adapter.aclient = Client()
    adapter.collection_name = 'collection-safety'
    adapter.content_dir = tmp_path / 'storage'
    adapter.vector_size = 3
    adapter._collection_ready = False
    adapter._collection_lock = asyncio.Lock()
    adapter._payload_index_task = None
    monkeypatch.setattr(support, '_qdrant_schema_mode', lambda: 'named')
    monkeypatch.setattr(support, 'index_contract_status', lambda: {'status': 'present'})
    monkeypatch.setattr(support, 'write_index_contract', lambda **_kwargs: None)
    monkeypatch.setattr(support, 'payload_index_ensure_enabled', lambda: False)
    monkeypatch.setattr('backend.qdrant_collection.recover_adapter', lambda _adapter: None)
    return adapter


@pytest.mark.parametrize('reported', [0, None])
async def test_zero_or_unknown_summary_cannot_authorize_deleting_real_points(collection, reported):
    collection.aclient.reported = reported
    with pytest.raises(EmbeddingContractError, match='explicit migration'):
        await collection._ensure_collection()
    assert collection.aclient.points == ['preserve-this-point']
    assert 'delete' not in collection.aclient.calls
    assert not collection._collection_ready


async def test_unavailable_exact_count_preserves_the_collection(collection):
    collection.aclient.count_failure = TimeoutError('unavailable')
    with pytest.raises(EmbeddingContractError):
        await collection._ensure_collection()
    assert 'delete' not in collection.aclient.calls


async def test_empty_legacy_migration_still_works_after_exact_confirmation(collection):
    collection.aclient.points.clear()
    await collection._ensure_collection()
    assert collection.aclient.calls.index('count') < collection.aclient.calls.index('delete')
    assert collection.aclient.calls.count('create') == 1
    assert collection._collection_ready


async def test_compatibility_does_not_adopt_unverified_points_as_empty(collection, monkeypatch):
    collection.aclient.compatible = True
    writes = []
    monkeypatch.setattr(support, 'index_contract_status', lambda: {'status': 'missing'})
    monkeypatch.setattr(support, 'write_index_contract', lambda **kwargs: writes.append(kwargs))
    # The unfiltered exact count is one, but none of those points prove the
    # embedding identity requested by the current connection.
    async def count(*, count_filter=None, exact, **_kwargs):
        return SimpleNamespace(count=0 if count_filter else 1)
    collection.aclient.count = count
    await collection._ensure_collection()
    assert writes == []
    assert collection.aclient.points == ['preserve-this-point']


async def test_startup_cannot_race_a_publication_or_recovery_writer(collection):
    with ReplacementJournal.for_adapter(collection).lease():
        with pytest.raises(EmbeddingContractError, match='INDEX_UPDATE_BUSY'):
            await collection._ensure_collection()
    assert 'delete' not in collection.aclient.calls


async def test_get_timeout_does_not_turn_into_create(collection):
    collection.aclient.get_failure = TimeoutError('offline')
    with pytest.raises(EmbeddingContractError):
        await collection._ensure_collection()
    assert 'create' not in collection.aclient.calls


async def test_real_404_allows_creation(collection):
    collection.aclient.points.clear()
    collection.aclient.get_failure = UnexpectedResponse(status_code=404, reason_phrase='Not Found', content=b'{}', headers={})
    await collection._ensure_collection()
    assert collection.aclient.calls.count('create') == 1


@pytest.mark.parametrize('missing_collection', [False, True])
async def test_missing_index_cannot_be_silently_recreated_over_indexed_catalog(collection, tmp_path, missing_collection):
    from backend.document_catalog import MetaDB
    db = MetaDB(str(tmp_path / 'meta.db'))
    dataset = db.create_dataset('Catalog fixture')
    db.add_document(dataset, 'source.txt')
    db.update_document_status(dataset, 'source.txt', 'INDEXED', 1)
    collection.db = db
    collection.aclient.points.clear()
    collection.aclient.compatible = True
    if missing_collection:
        collection.aclient.get_failure = UnexpectedResponse(status_code=404, reason_phrase='Not Found', content=b'{}', headers={})
    with pytest.raises(EmbeddingContractError, match='INDEX_CATALOG_MISMATCH'):
        await collection._ensure_collection()
    assert 'create' not in collection.aclient.calls and 'delete' not in collection.aclient.calls
    with db._get_conn() as conn:
        assert tuple(conn.execute('SELECT status, chunk_count FROM documents').fetchone()) == ('INDEXED', 1)
