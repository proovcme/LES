"""Regression coverage for review findings: lifecycle, migration and exact data."""
import sqlite3
from types import SimpleNamespace
import pytest
from backend.document_catalog import MetaDB
from backend.embedding_client import EmbedClient
from backend.interface import EmbeddingContractError
from proxy.services.chat_attachment_read_service import _format_tabular_attachment_context


def test_catalog_closes_connections_and_rolls_back_failed_writes(tmp_path):
    db=MetaDB(str(tmp_path/'catalog.db'))
    with pytest.raises(RuntimeError):
        with db._get_conn() as connection:
            connection.execute("INSERT INTO datasets(id,name) VALUES('test','Must roll back')")
            raise RuntimeError('interrupted')
    with pytest.raises(sqlite3.ProgrammingError): connection.execute('SELECT 1')
    with db._get_conn() as connection:
        assert connection.execute("SELECT count(*) FROM datasets WHERE id='test'").fetchone()[0]==0
    with pytest.raises(sqlite3.ProgrammingError): connection.execute('SELECT 1')


def test_catalog_migrates_old_schema_without_losing_source_records(tmp_path):
    path=tmp_path/'old.db'
    with sqlite3.connect(path) as conn:
        conn.executescript("CREATE TABLE datasets(id TEXT PRIMARY KEY,name TEXT,status TEXT); CREATE TABLE documents(id TEXT PRIMARY KEY,dataset_id TEXT,file_name TEXT,status TEXT);")
        conn.execute("INSERT INTO datasets VALUES('d','Лес','PENDING')")
        conn.execute("INSERT INTO documents VALUES('f','d','План 🌲.txt','PENDING')")
    MetaDB(str(path));MetaDB(str(path))
    with sqlite3.connect(path) as conn:
        assert conn.execute('SELECT file_name,status,chunk_count FROM documents').fetchall()==[('План 🌲.txt','PENDING',0)]
        assert conn.execute('SELECT name,sensitivity FROM datasets').fetchall()==[('Лес','P0')]


@pytest.mark.parametrize('model,vectors', [('bge-m3-other',[[1.0]]),('bge-m3',[[float('nan')]]),('bge-m3',[[float('inf')]])])
def test_embeddings_reject_wrong_model_and_non_finite_data(model,vectors):
    client=EmbedClient('http://127.0.0.1:11434',model='bge-m3')
    with pytest.raises(EmbeddingContractError):
        client._vectors_from_connection_response(SimpleNamespace(model_id=model,vectors=vectors),1)


def test_csv_attachment_preserves_quoted_newline_and_utf16(tmp_path):
    file=tmp_path/'План.csv'
    file.write_text('name;value\n"две\nстроки";"café"\n',encoding='utf-16', newline='')
    text,truncated=_format_tabular_attachment_context(file,file.name,max_chars=18000)
    assert 'CSV!R2:' in text
    assert '"coordinate":"A2"' in text and '"value":"две\\nстроки"' in text
    assert '"coordinate":"B2"' in text and '"value":"café"' in text
    assert not truncated


def test_metadb_list_datasets_counts_pending_as_files(tmp_path):
    from backend.qdrant_adapter import MetaDB

    db = MetaDB(str(tmp_path / "data" / "les_meta.db"))
    dataset_id = db.create_dataset("913")
    db.add_document(dataset_id, "913/doc.txt", file_mtime=1.0, file_size=12, source_path="/tmp/doc.txt")

    row = db.list_datasets()[0]

    assert row.name == "913"
    assert row.doc_count == 1
    assert row.files == 1
    assert row.indexed_files == 0
    assert row.pending_files == 1


def test_light_metadb_dataset_counts_exclude_temporary_office_files(tmp_path, monkeypatch):
    from backend.qdrant_adapter import MetaDB

    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    db = MetaDB(str(tmp_path / "data" / "les_meta.db"))
    dataset_id = db.create_dataset("project")
    db.add_document(dataset_id, "folder/~$draft.docx", file_mtime=1.0, file_size=20)
    db.add_document(dataset_id, "folder/report.pdf", file_mtime=1.0, file_size=20)
    db.update_document_status(dataset_id, "folder/~$draft.docx", "INDEXED", 4)
    db.update_document_status(dataset_id, "folder/report.pdf", "INDEXED", 3)

    row = db.list_datasets()[0]

    assert row.doc_count == row.files == row.indexed_files == 1
    assert row.chunk_count == 3


def test_metadb_requeues_existing_pdf_with_systemic_mojibake(tmp_path):
    from backend.qdrant_adapter import MetaDB
    from proxy.services.lexical_index_service import LexicalIndex

    db_path = tmp_path / "data" / "les_meta.db"
    db = MetaDB(str(db_path))
    dataset_id = db.create_dataset("NS")
    db.add_document(dataset_id, "project.pdf", file_mtime=1.0, file_size=12)
    db.update_document_status(dataset_id, "project.pdf", "INDEXED", 3)
    lexical = LexicalIndex(db_path=str(db_path))
    with lexical.connect() as conn:
        now = 1.0
        for index, text in enumerate((
            "ÐÐ»Ð°Ð½ ÑÑÐ°Ð¶Ð° Ð¸ ÑÐ¸ÑÑ",
            "Ð¡ÑÐµÐ¼Ð° ÑÐ»ÐµÐºÑÑÐ¾ÑÐ½Ð°Ð±Ð¶ÐµÐ½Ð¸Ñ",
            "ÐÐ°Ð±ÐµÐ»ÑÐ½ÑÐµ ÑÑÐ°ÑÑÑ",
        )):
            conn.execute(
                "INSERT INTO lexical_chunks(collection,point_id,dataset_id,doc_name,text,content_hash,updated_at) "
                "VALUES(?,?,?,?,?,?,?)",
                ("les_rag", f"p{index}", dataset_id, "project.pdf", text, f"h{index}", now),
            )

    names = db.requeue_corrupt_pdf_text_documents(dataset_id)

    assert names == ["project.pdf"]
    assert db.get_pending_files(dataset_id) == ["project.pdf"]
