from hashlib import sha256

from docx import Document
from docx.oxml import OxmlElement

from backend.interface import Chunk
from proxy.services.evidence_packet_service import build_retrieval_evidence_packet
from proxy.services.file_viewer_service import file_viewer_html
from proxy.services.word_evidence_service import retrieve_word_evidence
from tests.test_light_table_retrieval import catalog, register


def test_word_reads_full_small_section_and_skips_a_different_heading(tmp_path):
    backend = catalog(tmp_path, "ds", "other")
    path = tmp_path / "Правила # новая версия.docx"
    document = Document()
    document.add_heading("Порядок обработки", level=1)
    document.add_paragraph("Обычный срок FERN-44 составляет 10 дней.")
    document.add_page_break()
    document.add_paragraph().add_run("Важное условие").bold = True
    document.add_paragraph("Срочная заявка рассматривается за 3 дня.")
    document.add_heading("Другой порядок", level=1)
    document.add_paragraph("Чужой срок составляет 90 дней.")
    document.save(path)
    identity = register(backend.db, "ds", path.name, path)
    register(backend.db, "other", path.name, path)
    before = sha256(path.read_bytes()).hexdigest()

    rows, report = retrieve_word_evidence(
        backend, question="Какой срок FERN-44?", dataset_ids=["ds"],
        file_names=[path.name], index_hits=[],
    )
    assert [row.doc_id for row in rows] == [identity] * 4
    assert [row.meta["anchor"] for row in rows] == ["para0", "para1", "para3", "para4"]
    assert "90 дней" not in " ".join(row.content for row in rows)
    assert report[0]["group_blocks"] == report[0]["included_group_blocks"] == 4
    source = build_retrieval_evidence_packet(question="?", chunks=rows, retrieval_trace={}).source_map(max_chars=3000)[3]
    assert source["locator"]["anchor"] == "para4" and source["locator"]["relative_path"] == path.name
    preview = file_viewer_html(path, path_id=identity, locator="para4")
    assert 'id="para4" class="anchor evidence-hit"' in preview
    assert sha256(path.read_bytes()).hexdigest() == before


def test_word_reads_table_by_column_and_opens_row_after_first_preview_window(tmp_path):
    backend = catalog(tmp_path, "ds")
    path = tmp_path / "Колонки.docx"
    document = Document()
    document.add_heading("Ведомость", level=1)
    table = document.add_table(rows=1, cols=3)
    table.rows[0]._tr.get_or_add_trPr().append(OxmlElement("w:tblHeader"))
    header = table.rows[0].cells
    header[0].text = "Код"
    header[1].merge(header[2]).text = "Количество"
    for _ in range(510):
        table.add_row().cells[0].text = "Другой код"
    row = table.add_row().cells
    row[0].text, row[1].text, row[2].text = "FERN-77", "7654321", "комплект"
    document.save(path)
    identity = register(backend.db, "ds", path.name, path)
    hit = Chunk("Ведомость", "index-chunk", path.name, .9, {"dataset_id": "ds"})
    args = dict(question="Сколько FERN-77?", dataset_ids=["ds"], file_names=[], index_hits=[hit])
    rows, report = retrieve_word_evidence(backend, **args)
    assert rows[0].meta["anchor"] == "t0r511"
    assert "Столбец 2: 7654321" in rows[0].content and "Столбец 3: комплект" in rows[0].content
    assert any("Столбец 2–3: Количество" in item.content for item in rows)
    assert report[0]["included_group_blocks"] < report[0]["group_blocks"]
    preview = file_viewer_html(path, path_id=identity, locator="t0r511")
    assert 'id="t0r511" class="anchor evidence-hit"' in preview and "7654321" in preview
    path.write_bytes(path.read_bytes() + b" ")
    assert retrieve_word_evidence(backend, **args) == ([], [{"file_name": path.name, "status": "unavailable"}])
