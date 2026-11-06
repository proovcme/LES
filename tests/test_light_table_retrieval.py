from hashlib import sha256
from types import SimpleNamespace

from openpyxl import Workbook

from backend.document_catalog import MetaDB
from backend.interface import Chunk
from proxy.services.evidence_packet_service import build_retrieval_evidence_packet
from proxy.services.file_viewer_service import file_viewer_html
from proxy.services.table_retrieval_service import retrieve_table_rows


def register(db, dataset_id, name, path, *, status="INDEXED"):
    doc_id, _, _ = db.add_document(dataset_id, name, source_path=str(path) if path else "")
    if path:
        info = path.stat()
        db.set_document_source_fingerprint(
            dataset_id, name, file_hash=sha256(path.read_bytes()).hexdigest(),
            file_mtime=info.st_mtime, file_size=info.st_size,
        )
    with db._get_conn() as conn:
        conn.execute("UPDATE documents SET status=? WHERE id=?", (status, doc_id))
    return doc_id


def catalog(tmp_path, *datasets):
    db = MetaDB(str(tmp_path / "meta.db"))
    with db._get_conn() as conn:
        conn.executemany("INSERT INTO datasets (id, name) VALUES (?, ?)", [(item, item) for item in datasets])
    return SimpleNamespace(db=db, content_dir=tmp_path / "storage")


def test_normal_rag_can_find_rows_beyond_index_preview_on_multiple_sheets(tmp_path):
    backend = catalog(tmp_path, "active", "outside")
    path = tmp_path / "Книга с пробелами.xlsx"
    book = Workbook()
    book.active.title = "Часть 1"
    book.active.append(["Шифр", "Количество"])
    book.active.append(["ЭОМ1.2", 32])
    late = book.create_sheet("Часть 2")
    late.append(["Шифр", "Количество"])
    late["A2003"], late["C2003"], late["D2003"] = "ЭОМ1.2", 7654321, "='Часть 1'!B2+C2003"
    book.create_sheet("Прочее")["A1"] = "ЭОМ1.20"
    book.save(path)
    book.close()
    doc_id = register(backend.db, "active", path.name, path)
    register(backend.db, "outside", path.name, path)
    register(backend.db, "active", "Другое.xlsx", path)
    before = path.read_bytes()

    rows, coverage = retrieve_table_rows(
        backend, question="Какое количество указано для ЭОМ1.2?", dataset_ids=["active"],
        file_names=[path.name], index_hits=[],
    )
    assert [row.doc_id for row in rows] == [doc_id, doc_id]
    assert coverage["read"] == 1 and coverage["sheets"] == 3 and coverage["matched_rows"] == 2
    assert "C2003 = 7654321" in rows[1].content
    assert "результат формулы недоступен" in rows[1].content
    packet = build_retrieval_evidence_packet(question="ЭОМ1.2", chunks=rows, retrieval_trace={})
    locators = [item["locator"] for item in packet.source_map(max_chars=2000)]
    assert [(item["sheet"], item["row"], item["cell"]) for item in locators] == [
        ("Часть 1", 2, "A2:B2"), ("Часть 2", 2003, "A2003:D2003"),
    ]
    html = file_viewer_html(path, path_id=doc_id, sheet="Часть 2", locator="Часть 2!R2003")
    assert 'id="row-2003" class="anchor evidence-hit"' in html
    assert "7654321" in html
    assert path.read_bytes() == before


def test_csv_viewer_opens_late_row_with_original_encoding(tmp_path):
    path = tmp_path / "Таблица.csv"
    path.write_bytes(("Код;Количество\r\n" + "Строка;1\r\n" * 700 + "Линия;321\r\n").encode("utf-16"))
    html = file_viewer_html(path, path_id="csv", locator="CSV!R702")
    assert '<tr class="anchor evidence-hit"><th class="row-number">702</th><td>Линия</td><td>321</td>' in html


def test_prioritizes_retrieved_book_and_excludes_stale_or_pending_originals(tmp_path):
    backend = catalog(tmp_path, "active")
    source = tmp_path / "таблица.csv"
    source.write_text("Имя;Число\nTAG-001;123", encoding="utf-8")
    for index in range(5):
        register(backend.db, "active", f"{index}.csv", None)
    doc_id = register(backend.db, "active", source.name, source)
    register(backend.db, "active", "pending.csv", source, status="PENDING")
    hit = Chunk("Обзор таблицы", "synthetic-retrieval-id", source.name, 1, {"dataset_id": "active"})
    args = dict(question="TAG-001", dataset_ids=["active"], file_names=[], index_hits=[hit])
    rows, coverage = retrieve_table_rows(backend, **args)
    assert [row.doc_id for row in rows] == [doc_id]
    assert coverage["file_limit_reached"] is True
    source.write_text("Имя;Число\nTAG-001;999", encoding="utf-8")
    rows, coverage = retrieve_table_rows(backend, **args)
    assert rows == []
    assert "Версия оригинала не совпадает с индексом" in coverage["issues"]
