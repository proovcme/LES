from hashlib import sha256
from zipfile import ZipFile

from openpyxl import Workbook
import pytest

from proxy.services.evidence_packet_service import build_retrieval_evidence_packet
from proxy.services.table_retrieval_service import retrieve_table_rows
from proxy.services.tabular_document_service import read_table
from proxy.services.tool_harness_service import harness
from proxy.services.workbook_total_service import TOOL_NAME, calculate_ranges, register_workbook_totals, total_evidence
from tests.test_light_table_retrieval import catalog, register


def test_harness_sums_original_ranges_and_emits_each_source(tmp_path):
    backend = catalog(tmp_path, "active", "outside")
    path = tmp_path / "Книга с пробелами.xlsx"
    book = Workbook()
    book.active.title = "Лист 1"
    for index in range(2, 602):
        book.active[f"B{index}"] = 0.1
    book.create_sheet("Лист 2")["D3"] = 0.2
    book.save(path)
    book.close()
    original = path.read_bytes()
    doc_id = register(backend.db, "active", path.name, path)
    outside_id = register(backend.db, "outside", path.name, path)
    _, coverage = retrieve_table_rows(
        backend, question="Сумма по двум листам", dataset_ids=["active"], file_names=[], index_hits=[])
    candidates = coverage["xlsx_sources"]
    tool_harness = harness()
    register_workbook_totals(tool_harness, backend, candidates)
    assert tool_harness.call(TOOL_NAME, {"operation": "list"})["result"]["workbooks"] == [
        {"doc_id": doc_id, "name": path.name}]
    args = {"operation": "sum", "doc_id": doc_id, "ranges": [
        {"sheet": "Лист 1", "range": "$B$2:$B$602"}, {"sheet": "Лист 2", "range": "D3"}]}
    response = tool_harness.call(TOOL_NAME, args)
    assert response["status"] == "ok", response
    result = response["result"]
    assert result["total"] == "60.2"
    assert result["source_sha256"] == sha256(original).hexdigest()
    assert [(part["sum"], part["numeric_cells"], part["blank_cells"]) for part in result["ranges"]] == [
        ("60", 600, 1), ("0.2", 1, 0)]
    chunks = total_evidence(response, candidates)
    packet = build_retrieval_evidence_packet(question="Сколько?", chunks=list(chunks), retrieval_trace={})
    assert [(source["locator"]["sheet"], source["locator"]["cell"]) for source in packet.source_map(max_chars=2000)] == [
        ("Лист 1", "B2:B602"), ("Лист 2", "D3")]
    assert tool_harness.call(TOOL_NAME, {**args, "doc_id": outside_id})["status"] == "error"
    assert path.read_bytes() == original
    path.write_bytes(original + b" ")
    assert tool_harness.call(TOOL_NAME, args)["status"] == "error"


def test_invalid_ranges_and_unresolved_values_refuse_the_total(tmp_path):
    path = tmp_path / "invalid.xlsx"
    book = Workbook()
    sheet = book.active
    sheet["A1"], sheet["A2"], sheet["B1"] = 1, "=A1+1", "2"
    sheet.merge_cells("C1:C2")
    book.save(path)
    book.close()
    snapshot = read_table(path)
    def ranges(*addresses):
        return [{"sheet": sheet.title, "range": address} for address in addresses]
    with pytest.raises(ValueError, match="пересекаются"):
        calculate_ranges(snapshot, ranges("A1:B1", "B1"))
    with pytest.raises(ValueError, match="нет сохранённого результата"):
        calculate_ranges(snapshot, ranges("A1:A2"))
    with pytest.raises(ValueError, match="нельзя сложить"):
        calculate_ranges(snapshot, ranges("B1"))
    with pytest.raises(ValueError, match="объединённые"):
        calculate_ranges(snapshot, ranges("C2"))

    cached = tmp_path / "cached.xlsx"
    with ZipFile(path) as source, ZipFile(cached, "w") as target:
        for member in source.infolist():
            raw = source.read(member)
            if member.filename == "xl/worksheets/sheet1.xml":
                raw = raw.replace(b"<f>A1+1</f><v></v>", b"<f>A1+1</f><v>2</v>")
            target.writestr(member, raw)
    result = calculate_ranges(read_table(cached), ranges("A1:A2"))
    assert result["total"] == "3" and result["ranges"][0]["cached_formulas"] == 1
