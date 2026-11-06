"""Contracts crossing the native writer and the main chat tool loop."""
from contextlib import closing

import pytest

from backend import native_core
from backend.interface import Chunk
from proxy.services.chat_prompt_support import _parse_model_tool_calls
from proxy.services.harness_loop_service import admitted_tool_evidence
from proxy.services.lexical_index_service import LexicalIndex


def test_native_writer_rollback_keeps_fts_and_does_not_steal_another_file_point(tmp_path):
    assert native_core.supports("lexical_replace")
    index = LexicalIndex(str(tmp_path / "catalog.sqlite"))
    original = dict(point_id="prior", dataset_id="ds", doc_name="Лист.pdf", text="действующий")
    foreign = dict(point_id="other", dataset_id="other-ds", doc_name="Лист.pdf", text="соседний")
    index.upsert_chunks("docs", [original, foreign])
    valid = {**original, "point_id": "new", "text": "замена", "parent_id": "родитель", "chunk_ord": 7}
    with pytest.raises(native_core.NativeCoreError, match="LEXICAL_POINT_SCOPE_CONFLICT"):
        index.replace_file("docs", dataset_id="ds", doc_name="Лист.pdf",
                           rows=[valid, {**valid, "point_id": "other"}])
    with closing(index.connect()) as conn:
        assert sorted(tuple(row) for row in conn.execute(
            "SELECT c.point_id,c.text FROM lexical_chunks_fts f "
            "JOIN lexical_chunks c ON c.id=f.rowid ORDER BY c.point_id"
        ).fetchall()) == [("other", "соседний"), ("prior", "действующий")]
        assert conn.execute("SELECT count(*) FROM lexical_chunks_fts WHERE lexical_chunks_fts MATCH 'замена'").fetchone()[0] == 0
    assert index.replace_file("docs", dataset_id="ds", doc_name="Лист.pdf", rows=[valid]) == 1
    with closing(index.connect()) as conn:
        assert tuple(conn.execute(
            "SELECT c.point_id,c.parent_id,c.chunk_ord FROM lexical_chunks_fts f "
            "JOIN lexical_chunks c ON c.id=f.rowid WHERE lexical_chunks_fts MATCH 'замена'"
        ).fetchone()) == ("new", "родитель", 7)
        assert conn.execute("SELECT count(*) FROM lexical_chunks_fts WHERE lexical_chunks_fts MATCH 'действующий'").fetchone()[0] == 0


def test_harness_preserves_exact_arguments_and_rejects_unlisted_tools():
    raw = 'Результат:\n' + chr(96) * 3 + 'JSON\n' + (
        '{"calls":[{"tool":"unknown","args":{}},{"name":"read_source","call_id":"чтение-1",'
        '"args":{"row":340282366920938463463374607431768211456,"path":"Книга 東京"}},'
        '{"tool":"read_source","args":{}}]}'
    ) + '\n' + chr(96) * 3
    assert _parse_model_tool_calls(raw, allowed_tools={"read_source"}, max_calls=1) == [
        {"tool": "read_source", "call_id": "чтение-1",
         "args": {"row": 340282366920938463463374607431768211456, "path": "Книга 東京"}}
    ]


def test_harness_keeps_exact_original_and_deduplicates_new_evidence():
    prior = Chunk("прочитано", "", "Скан.pdf", 0.9, {"dataset_id": "ds"})
    replacement = Chunk("общий результат поиска", "", "Книга.xlsx", 0.8, {"dataset_id": "ds"})
    other = Chunk("другое доказательство", "", "Книга.xlsx", 0.8, {"dataset_id": "other"})
    assert admitted_tool_evidence([prior], [prior, replacement, other, other],
                                  {("ds", "Книга.xlsx")}) == [other]
