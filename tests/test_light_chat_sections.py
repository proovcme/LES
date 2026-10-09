"""Real indexed parent reads through the production evidence/token boundary."""
import uuid
from types import SimpleNamespace

import pytest
from fastapi import HTTPException
from qdrant_client import AsyncQdrantClient, models

from backend.interface import Chunk
from backend.index_replacement import ReplacementJournal
from proxy.services.chat_section_context_service import ChatSectionReader, visible_chunks
from proxy.services.chat_evidence_context import govern_inference_messages, model_visible_source_map, source_context_blocks
from proxy.services.evidence_packet_service import build_retrieval_evidence_packet, render_retrieval_evidence_for_model
from proxy.services.context_governor_service import ContextKind
from proxy.services.public_error_service import public_error_payload
from tests.test_context_governor_service import _preset


def point(text, ordinal=0, dataset="ds", name="Раздел с пробелами.pdf", parent="p"):
    return models.PointStruct(id=str(uuid.uuid4()), vector=[1., 0.], payload={
        "dataset_id": dataset, "file_name": name, "parent_id": parent,
        "chunk_ord": ordinal, "page": ordinal + 1, "text": text, "doc_id": "document",
        "node_role": "evidence", "source_ref": f"{name}#page={ordinal + 1}",
    })


def chunk(point):
    p = point.payload
    return Chunk(p["text"], p["doc_id"], p["file_name"], .9, {**p, "qdrant_point_id": str(point.id)})


async def backend(tmp_path, points):
    client = AsyncQdrantClient(":memory:")
    await client.create_collection("docs", vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
    await client.upsert("docs", points=points)
    return SimpleNamespace(aclient=client, content_dir=tmp_path, collection_name="docs")


@pytest.mark.asyncio
async def test_parent_exception_reaches_model_with_own_page_and_exact_quote(tmp_path):
    first = point("Основное требование — выполнить проверку.")
    exception = point("Исключение: при температуре ниже минус 20 °C работы прекращают.", 1)
    alien = point("Чужое требование", 2, dataset="other")
    b = await backend(tmp_path, [first, exception, alien])
    result = await ChatSectionReader(b, ["ds"]).expand([chunk(first)])
    assert result.added_count == 1
    assert [c.meta["page"] for c in result.chunks] == [1, 2]
    packet = build_retrieval_evidence_packet(question="Условия?", chunks=result.chunks, retrieval_trace={})
    context = render_retrieval_evidence_for_model(packet, max_chars=5000)
    sources = packet.source_map(max_chars=5000)
    messages, bounded = govern_inference_messages(
        preset=_preset(limit=4000), profile_prefix="Use evidence", request_payload="Условия?",
        evidence=source_context_blocks(context), source_map=sources)
    assert exception.payload["text"] in str(messages)
    visible = model_visible_source_map(bounded, sources)
    assert visible[1]["qdrant_point_id"] == str(exception.id)
    assert visible[1]["page"] == 2
    assert visible[1]["quote"] == exception.payload["text"]
    assert "Чужое" not in str(messages)
    await b.aclient.close()


@pytest.mark.asyncio
async def test_seed_priority_and_atomic_budget_do_not_clip_exceptions(tmp_path):
    seed = point("Найденный ответ")
    huge = point("Исключение " * 500, 1)
    second = point("Другой важный ответ", parent="second", name="Другой.pdf")
    b = await backend(tmp_path, [seed, huge, second])
    result = await ChatSectionReader(b, ["ds"]).expand([chunk(seed), chunk(second)])
    assert [c.content for c in result.chunks[:2]] == [seed.payload["text"], second.payload["text"]]
    packet = build_retrieval_evidence_packet(question="?", chunks=result.chunks, retrieval_trace={})
    context = render_retrieval_evidence_for_model(packet, max_chars=800)
    sources = packet.source_map(max_chars=800)
    assert len(sources) == 2
    assert "Исключение" not in context
    assert all(source["context_origin"] == "search_hit" for source in sources)
    await b.aclient.close()


@pytest.mark.asyncio
async def test_reader_paginates_and_keeps_parent_scope(tmp_path):
    points = [point(f"Фрагмент {i}", i) for i in range(140)]
    wrong_file = point("other", name="other.pdf")
    nav = point("navigation")
    nav.payload["node_role"] = "navigation"
    b = await backend(tmp_path, [*points, wrong_file, nav])
    result = await ChatSectionReader(b, ["ds"]).expand([chunk(points[80])])
    assert result.added_count == 139
    assert result.sections[0]["indexed_fragments"] == 140
    assert {c.meta["qdrant_point_id"] for c in result.chunks} == {str(p.id) for p in points}
    await b.aclient.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("mutation", ["text", "page", "deleted", "scope"])
async def test_stale_or_out_of_scope_hits_stop_before_generation(tmp_path, mutation):
    p = point("Original")
    original = chunk(p)
    b = await backend(tmp_path, [p])
    if mutation == "deleted":
        await b.aclient.delete("docs", points_selector=models.PointIdsList(points=[p.id]))
    elif mutation != "scope":
        await b.aclient.set_payload("docs", payload={mutation: "changed" if mutation == "text" else 99}, points=[p.id])
    with pytest.raises(HTTPException) as err:
        await ChatSectionReader(b, ["other"] if mutation == "scope" else ["ds"]).expand([original])
    assert err.value.status_code == 409
    await b.aclient.close()


@pytest.mark.asyncio
async def test_update_between_read_and_inference_is_rejected(tmp_path):
    p = point("Original")
    b = await backend(tmp_path, [p])
    reader = ChatSectionReader(b, ["ds"])
    await reader.expand([chunk(p)])
    journal = ReplacementJournal.for_adapter(b)
    with journal.lease():
        journal.begin("ds", p.payload["file_name"], "original.pdf", [str(uuid.uuid4())])
    with pytest.raises(HTTPException) as err:
        reader.assert_for_inference()
    assert err.value.detail["code"] == "INDEX_RECOVERY_REQUIRED"
    await b.aclient.close()


@pytest.mark.asyncio
async def test_parent_read_failure_is_actionable_not_hidden(tmp_path):
    p = point("Original")
    b = await backend(tmp_path, [p])
    async def broken(**kwargs):
        raise OSError("private internal path")
    b.aclient.scroll = broken
    with pytest.raises(HTTPException) as err:
        await ChatSectionReader(b, ["ds"]).expand([chunk(p)])
    public = public_error_payload(status_code=err.value.status_code, detail=err.value.detail)
    assert public["code"] == "SECTION_READ_UNAVAILABLE"
    assert "дочитать" in public["detail"]
    assert "private" not in public["detail"]
    await b.aclient.close()


def test_dropped_evidence_cannot_leave_dangling_model_source_reference():
    evidence = ["[Источник 1 | huge.pdf]:\n" + "X" * 2000, "[Источник 2 | small.pdf]:\nSmall"]
    sources = [{"index": 1, "label": "Источник 1", "doc_name": "huge.pdf"},
               {"index": 2, "label": "Источник 2", "doc_name": "small.pdf"}]
    messages, packet = govern_inference_messages(
        preset=_preset(limit=500, generation=100, safety=50),
        profile_prefix="Grounded", request_payload="Question",
        evidence=evidence, source_map=sources)
    assert "huge.pdf" not in str(messages)
    assert "small.pdf" in str(messages)
    assert packet.included_tokens <= packet.input_budget_tokens
    assert any(o.kind == ContextKind.EVIDENCE for o in packet.omissions)


def test_ui_previews_cannot_restore_evidence_dropped_by_governor():
    p, other = point("Included"), point("Dropped", 1)
    chunks = [chunk(p), chunk(other)]
    sources = [{"dataset_id": "ds", "doc_name": p.payload["file_name"],
                "qdrant_point_id": str(p.id), "quote": "Included"}]
    assert visible_chunks(chunks, sources) == chunks[:1]
    assert visible_chunks(chunks, []) == []
