"""Regression coverage for review findings: lifecycle, migration and exact data."""
import sqlite3
from types import SimpleNamespace
import pytest
from backend.document_catalog import MetaDB
from backend.embedding_client import EmbedClient
from backend.interface import EmbeddingContractError
from proxy.services.dataset_watch_service import DatasetWatcher
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


def test_watch_batch_keeps_last_thousand_events_and_other_datasets(tmp_path):
    watch=DatasetWatcher(tmp_path/'watch.db')
    with watch.connect() as conn:
        watch._event(conn,'other','new','preserved.txt','Added')
        watch._events(conn,'test',(('new',f'{i}.txt','Added') for i in range(3000)))
        assert conn.execute("SELECT count(*) FROM events WHERE dataset_id='test'").fetchone()[0]==1000
        assert conn.execute("SELECT file_name FROM events WHERE dataset_id='test' ORDER BY id LIMIT 1").fetchone()[0]=='2000.txt'
    assert watch.status('other')['events'][0]['file_name']=='preserved.txt'


def test_csv_attachment_preserves_quoted_newline_and_utf16(tmp_path):
    file=tmp_path/'План.csv'
    file.write_text('name;value\n"две\nстроки";"café"\n',encoding='utf-16')
    text,truncated=_format_tabular_attachment_context(file,file.name,max_chars=18000)
    assert 'CSV!R2: две строки | café' in text
    assert not truncated
