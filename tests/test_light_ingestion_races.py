"""Publication and prefetch regressions with a real SQLite source catalog."""
from types import SimpleNamespace

import pytest

from backend import qdrant_support as support
from backend.document_catalog import MetaDB
from backend.document_router import route_document
from backend.index_replacement import ReplacementJournal
from backend.qdrant_ingestion import QdrantIngestion
from backend.sparse_index import external_mutation


@pytest.fixture
def ingestion(tmp_path, monkeypatch):
    db = MetaDB(str(tmp_path / 'meta.db'))
    dataset = db.create_dataset('Review fixture')
    storage = tmp_path / 'storage'
    (storage / dataset).mkdir(parents=True)
    sources = tmp_path / 'Источники с пробелами'
    sources.mkdir()
    published = []

    class Client:
        def __init__(self, *args, **kwargs): pass
        def upsert(self, **kwargs): published.extend(kwargs['points'])
        def delete(self, **kwargs): pass

    def convert(adapter, path, *args):
        return route_document(path), [{'text': path.read_text(encoding='utf-8'), 'payload': {}}]

    def add(name):
        source = sources / name
        source.write_text('Approved delivery January 2031.', encoding='utf-8')
        db.add_document(dataset, name, source_path=str(source))
        return source

    adapter = SimpleNamespace(
        content_dir=storage, db=db, collection_name='review', qdrant_url='http://127.0.0.1:1',
        embed=SimpleNamespace(encode_sync=lambda texts: [[.1] * 1024 for _ in texts]),
        _sync_count_file_points=lambda client, ds, name: sum(p.payload['file_name'] == name for p in published),
    )
    monkeypatch.setattr(support.qdrant_client, 'QdrantClient', Client)
    monkeypatch.setattr(support, 'qdrant_client_options', lambda *_: {})
    monkeypatch.setattr(QdrantIngestion, '_convert_file', convert)
    monkeypatch.setattr(support, 'PARSE_PREFETCH', True)
    monkeypatch.setenv('RAG_QDRANT_SCHEMA', 'named')
    monkeypatch.setenv('RAG_CHUNK_UNIT', 'chars')
    return adapter, dataset, add, published


@pytest.mark.parametrize('mutation', ['dataset', 'document', 'recreate', 'relocate'])
def test_publication_rejects_removed_or_replaced_registration(ingestion, mutation):
    adapter, dataset, add, published = ingestion
    source = add('doc.md')
    original_id = adapter.db.document_registration(dataset, 'doc.md')[0]
    journal = ReplacementJournal.for_adapter(adapter)

    def encode(texts):
        # Exercise the same lease/journal boundary as safe dataset deletion.
        with external_mutation(journal, dataset=dataset):
            if mutation == 'dataset':
                from proxy.services.dataset_deletion_service import _delete_catalog_rows
                _delete_catalog_rows(adapter.db.db_path, [dataset])
            elif mutation in {'document', 'recreate'}:
                with adapter.db._get_conn() as conn:
                    conn.execute('DELETE FROM documents WHERE dataset_id=?', (dataset,))
                if mutation == 'recreate':
                    adapter.db.add_document(dataset, 'doc.md', source_path=str(source))
            else:
                moved = source.with_name('moved.md')
                moved.write_bytes(source.read_bytes())
                adapter.db.add_document(dataset, 'doc.md', source_path=str(moved))
        return [[.1] * 1024 for _ in texts]

    adapter.embed.encode_sync = encode
    result = QdrantIngestion._sync_parse(adapter, dataset, limit=1)
    assert result['errors'] == 1
    assert published == []
    assert source.is_file()  # External originals survive dataset removal.
    assert journal.pending() is None
    journal.assert_clean()  # No collection-wide recovery blockade.
    rows = adapter.db.dataset_integrity_rows(dataset)
    if mutation in {'dataset', 'document'}:
        assert rows == []
    else:
        assert rows[0]['status'] == 'PENDING'
        if mutation == 'recreate':
            assert rows[0]['id'] != original_id


@pytest.mark.parametrize('missing', ['a.md', 'b.md'])
def test_missing_prefetch_source_does_not_poison_other_files(ingestion, monkeypatch, missing):
    adapter, dataset, add, published = ingestion
    sources = {name: add(name) for name in ('a.md', 'b.md', 'c.md')}
    # Remove after the pending list was captured, as a watcher/user can do.
    original_fingerprint = QdrantIngestion._source_fingerprint
    def fingerprint(path):
        if path.name == 'a.md' and sources[missing].exists():
            sources[missing].unlink()
        return original_fingerprint(path)
    monkeypatch.setattr(QdrantIngestion, '_source_fingerprint', staticmethod(fingerprint))
    result = QdrantIngestion._sync_parse(adapter, dataset, limit=3)
    assert result['errors'] == 1
    rows = {row['file_name']: row for row in adapter.db.dataset_integrity_rows(dataset)}
    assert rows[missing]['status'] == 'ERROR'
    for name in sources.keys() - {missing}:
        assert rows[name]['status'] == 'INDEXED'
    assert {p.payload['file_name'] for p in published} == sources.keys() - {missing}
    assert ReplacementJournal.for_adapter(adapter).pending() is None
