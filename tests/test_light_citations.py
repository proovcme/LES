"""Human citation labels, full proof and exact per-answer navigation."""
import json
import shutil
import subprocess

import pytest

from sovushka.answer_render import citation_drawer_item, link_source_markers, source_chip, source_marker_numbers, source_usage
from sovushka.components.source_links import SOURCE_LINK_CLICK


def test_model_named_marker_links_to_verified_document_name_and_page():
    source = {"doc_id": "document", "doc_name": "Архив/План открытия.pdf", "page": 3,
              "source_ref": "internal/document.pdf#p3", "snippet": "Открытие 17 мая."}
    text = "Открытие 17 мая [Источник 1 | выдуманное имя 2031.pdf]."
    result = link_source_markers(text, source_count=1, sources=[source], anchor_prefix="source-42")
    assert result == "Открытие 17 мая [План открытия.pdf · стр.3](#source-42-1)."
    assert source_usage(source, 1, text)["code"] == "used"
    assert source_marker_numbers(text) == [1]


def test_grouped_citations_have_a_distinct_link_for_each_proof():
    result = link_source_markers("Факт [Источники 1, 2 | документы].", source_count=2,
                                 sources=[{"doc_name": "А.pdf"}, {"doc_name": "Б.pdf"}])
    assert "[А.pdf](#source-1), [Б.pdf](#source-2)" in result
    assert source_marker_numbers("[Источники 1 | 2]") == [1, 2]


def test_missing_citation_stays_unlinked_and_document_label_cannot_inject_markdown():
    result = link_source_markers("[Источник 1] [Источник 9]", source_count=1,
                                 sources=[{"doc_name": r"C:\private\Имя[скобки]*.pdf"}])
    assert r"[Имя\[скобки\]\*.pdf](#source-1)" in result
    assert result.endswith("[Источник 9]")
    assert "private" not in result
    assert source_chip({"doc_name": r"C:\private\А.pdf"})["file"] == "А.pdf"


def test_proof_drawer_retains_text_after_compact_preview_limit():
    excerpt = "Контекст. " * 45 + "Точное доказательство: 17 мая 2031 года."
    item = citation_drawer_item({"doc_id": "id", "doc_name": "План.txt", "snippet": excerpt})
    assert item["snippet"] == excerpt
    assert item["open_url"].startswith("/lite-api/documents/by-id/id/raw")


@pytest.mark.skipif(shutil.which("node") is None, reason="Node is needed for the DOM event regression")
def test_inline_citation_opens_exact_drawer_without_following_foreign_links():
    script = "const handler = " + SOURCE_LINK_CLICK + ";\n" + r'''
const assert = require('node:assert/strict');
for (const kind of ['citation', 'external', 'missing', 'unrelated']) {
    let clicks = 0, prevented = 0;
    const target = {classList: {contains: c => kind !== 'unrelated' && c === 'sov-ui-source-chip'}, click: () => clicks++};
    global.document = {getElementById: id => {assert.equal(id, 'source-42-2'); return kind === 'missing' ? null : target;}};
    const link = {getAttribute: () => '#source-42-2'};
    handler({target: {closest: () => kind === 'external' ? null : link}, preventDefault: () => prevented++});
    assert.equal(clicks, kind === 'citation' ? 1 : 0);
    assert.equal(prevented, kind === 'citation' ? 1 : 0);
}
'''
    subprocess.run([shutil.which("node"), "-e", script], check=True, capture_output=True, text=True)
