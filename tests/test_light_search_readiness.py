"""Native index evidence must agree with the actual rendered catalog."""
from types import SimpleNamespace

import pytest
from nicegui import Client, ui
from nicegui.page import page

from proxy.services import rag_readiness_service as readiness


def test_light_readiness_does_not_require_removed_estimate_modules(monkeypatch):
    class Index:
        def get_aliases(self):
            return SimpleNamespace(aliases=[])

        def collection_exists(self, name):
            return True

        def count(self, *args, **kwargs):
            return SimpleNamespace(count=2)

        def close(self):
            pass

    monkeypatch.setattr(readiness, "QdrantClient", lambda **kwargs: Index())
    monkeypatch.setenv("QDRANT_URL", "http://127.0.0.1:16339")
    monkeypatch.setenv("LES_LIGHT_QDRANT_URL", "http://127.0.0.1:16339")
    monkeypatch.setenv("LES_LIGHT_QDRANT_API_KEY", "synthetic-fixture-key")
    monkeypatch.setattr(readiness, "rag_collection_name", lambda: "les_rag")
    monkeypatch.setattr(readiness, "_source_chunks", lambda dataset_id: 2)
    monkeypatch.setattr(readiness, "_lexical_status", lambda *args: {})
    monkeypatch.setattr(readiness, "index_contract_status", lambda: {
        "compatible": True, "actual": {"point_embedding_fingerprint": "fixture"},
    })
    monkeypatch.setattr(readiness, "load_policy", lambda: {})
    monkeypatch.setattr(readiness, "load_status", lambda: {})
    result = readiness.rag_readiness(dataset_id="fixture", force=True)
    assert result["status"] == "ok"
    assert result["general"]["rrf_ready"] is True
    assert result["general"]["points"] == 2
    assert "smeta" not in result


@pytest.mark.asyncio
@pytest.mark.parametrize("chunks, index_state, label, ready", [
    (0, "ready", "Нет фрагментов у 1 файлов", False),
    (2, "missing", "Поисковый индекс не подтверждён", False),
    (2, "error", "Поисковый индекс не подтверждён", False),
    (2, "mismatch", "Поисковый индекс не подтверждён", False),
    (2, "ready", "Готов к поиску", True),
])
async def test_catalog_counts_badges_and_ready_filter_require_index_evidence(
    monkeypatch, chunks, index_state, label, ready,
):
    from sovushka.pages import samovar
    from tests.test_light_ui_handlers import click

    timers, calls = [], []

    async def get(route):
        calls.append(route)
        if route == "/api/rag/datasets":
            # A stale dataset total must not replace the observed zero chunks.
            return [{"id": "fixture", "name": "Fixture", "chunk_count": 99}]
        if route.startswith("/api/rag/documents?"):
            return {"documents": [{"dataset_id": "fixture", "status": "INDEXED", "chunk_count": chunks}]}
        if route.startswith("/api/rag/readiness?"):
            return {"status": "error" if index_state == "error" else "ok", "general": {
                "rrf_ready": index_state in {"ready", "mismatch"}, "activated": True,
                "points": 1 if index_state == "mismatch" else chunks,
            }}
        return {}

    monkeypatch.setattr(samovar, "api_get", get)
    monkeypatch.setattr(ui, "timer", lambda interval, callback, **kwargs: timers.append(callback))
    with Client(page("/__readiness")) as client:
        samovar.build_samovar(can_manage=False)
        await timers[0]()
        texts = [str(getattr(e, "text", "")) for e in client.elements.values()]
        assert label in texts
        summary = [e.text for e in client.elements.values()
                   if "sov-dataset-summary__value" in e._classes]
        assert summary == ["1", "1", "1" if ready else "0", "0", "0", str(chunks)]
        if chunks:
            assert "/api/rag/readiness?dataset_id=fixture" in calls
        button = next(e for e in client.elements.values() if isinstance(e, ui.button) and e.text == "Готовы")
        await click(button)
        names = [e.text for e in client.elements.values() if "sov-dataset-row__name" in e._classes]
        assert names == (["Fixture"] if ready else [])
