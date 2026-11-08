"""Durable catalog/queue behavior across independent SQLite connections."""
import os
import sqlite3
from pathlib import Path

import pytest

from backend import native_core
from backend.document_catalog import MetaDB
from backend.pdf_reader import _native_markdown, extract_markdown
from backend.workspace_native import catalog_call
from proxy.services.chat_session_service import SessionConflictError
from tests.pdf_fixtures import write_pdf


def test_registration_replays_only_uncommitted_files_and_preserves_ready_documents(tmp_path):
    database = tmp_path / "catalog.db"
    catalog = MetaDB(str(database))
    with sqlite3.connect(database) as connection:
        connection.execute("INSERT INTO datasets(id,name) VALUES('ds','Набор')")
        connection.execute(
            "INSERT INTO documents(id,dataset_id,file_name,status,file_size,file_mtime,source_path,chunk_count) "
            "VALUES('known','ds','Файл 0.pdf','INDEXED',1,2,'source0',9)"
        )

    def call(action, **args):
        return catalog_call(database, "intake_" + action, **args)

    args = {"root": "source", "dataset_id": "ds", "parse_limit": 2}
    job = call("enqueue", **args)
    job_id = job["id"]
    assert call("enqueue", **args)["id"] == job_id
    with pytest.raises(SessionConflictError):
        call("enqueue", **{**args, "root": "other"})
    assert call("claim")["resumed"] is False
    call("manifest", id=job_id, root="source", files=[
        {"file_name": f"Файл {i}.pdf", "source_path": f"source{i}"} for i in range(3)
    ])
    files = [
        {**row, "file_size": 1, "file_mtime": 2.0}
        for row in call("batch", id=job_id)
    ]
    with pytest.raises(SessionConflictError):
        call("register", id=job_id, files=[files[0], {**files[1], "ordinal": 12}])
    assert call("get", id=job_id)["processed"] == 0
    call("register", id=job_id, files=files[:1])

    # Another invocation of the leased worker resumes the same persisted job.
    resumed = call("claim")
    assert (resumed["processed"], resumed["recoveries"], resumed["reason_code"]) == (1, 1, "INTAKE_INTERRUPTED")
    assert [item["ordinal"] for item in call("batch", id=job_id)] == [1, 2]
    call("register", id=job_id, files=files[1:])
    with sqlite3.connect(database) as connection:
        assert connection.execute(
            "SELECT status,chunk_count FROM documents WHERE id='known'"
        ).fetchone() == ("INDEXED", 9)
        connection.execute(
            "INSERT INTO documents(id,dataset_id,file_name,status,file_size,source_path) "
            "VALUES('other','ds','Другое задание.pdf','PENDING',1,'elsewhere')"
        )
        connection.execute("UPDATE documents SET stage='EMBED' WHERE file_name='Файл 1.pdf'")
    assert call("index_batch", id=job_id) == [
        {"file_name": "Файл 1.pdf", "source_path": "source1"},
        {"file_name": "Файл 2.pdf", "source_path": "source2"},
    ]
    progress = call("get", id=job_id)
    assert (progress["current_stage"], progress["current_file"]) == ("EMBED", "Файл 1.pdf")
    sources = [(f"Файл {i}.pdf", f"source{i}") for i in (1, 2)]
    visits = []

    def process(index, attempts):
        visits.append((index, attempts))
        if index == 0:
            with sqlite3.connect(database) as connection:
                # Simulate a source being rebound while the first one is processed.
                connection.execute("UPDATE documents SET source_path='replacement' WHERE file_name='Файл 2.pdf'")
        return 0

    with pytest.raises(native_core.NativeCoreError, match="INDEX_BATCH_SELECTION_CHANGED"):
        native_core.dispatch_index_batch(database, job_id, sources[::-1], process)
    assert native_core.dispatch_index_batch(database, job_id, sources, process) == {"attempted": 1, "interrupted": False}
    assert visits == [(0, 1), (1, 0)]
    manual = []
    assert native_core.dispatch_index_batch(database, None, [
        ("Другое задание.pdf", "wrong"), ("Другое задание.pdf", "elsewhere"),
    ], lambda index, attempts: manual.append((index, attempts)) or 0,
        dataset_id="ds") == {"attempted": 1, "interrupted": False}
    assert manual == [(0, 0), (1, 1)]
    with sqlite3.connect(database) as connection:
        connection.execute("UPDATE documents SET source_path='source2' WHERE file_name='Файл 2.pdf'")
        connection.execute("UPDATE documents SET status='SKIPPED' WHERE file_name='Файл 1.pdf'")
        connection.execute("UPDATE documents SET status='INDEXED',chunk_count=0 WHERE file_name='Файл 2.pdf'")
    result = call("index_checkpoint", id=job_id, reason="", message="")
    assert result == {"status": "failed", "indexed": 1, "errors": 1, "pending": 0, "skipped": 1}
    final = call("get", id=job_id)
    assert final["reason_code"] == "INDEX_DOCUMENT_ERRORS"
    assert final["recoveries"] == 1
    assert call("claim") is None


def test_native_pdf_keeps_real_page_numbers_and_scan_detection(tmp_path, monkeypatch):
    import pypdfium2_raw

    api = os.getenv("LES_TEST_NATIVE_API")
    asset = Path(pypdfium2_raw.__file__).with_name("pdfium.dll")
    if not api or not asset.is_file() or not native_core.supports("worker_pdf"):
        pytest.skip("Build the Windows native PDF worker for this check")
    monkeypatch.delenv("LES_NATIVE_PDF_WORKER", raising=False)
    path = write_pdf(tmp_path / "Лист 東京.pdf", ["First readable page", None, "Last"])
    expected = extract_markdown(path, tables=False)
    assert expected == ("## Стр. 1\n\nFirst readable page\n\n## Стр. 3\n\nLast", True)
    monkeypatch.setenv("LES_NATIVE_PDF_WORKER", api)
    monkeypatch.setenv("LES_NATIVE_PDFIUM", str(asset))
    try:
        assert _native_markdown(path) == expected
    finally:
        native_core.required("worker_stop")
