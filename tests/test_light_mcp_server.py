"""Behavior of the independent Light MCP boundary."""
import asyncio
import json

import pytest

from tools import light_mcp_server as server


def test_running_api_uses_ready_light_state_only(tmp_path, monkeypatch):
    monkeypatch.setenv("LES_LIGHT_STATE", str(tmp_path))
    with pytest.raises(RuntimeError, match="не запущен"):
        server._running_api()
    status = tmp_path / "launcher-status.json"
    status.write_text(json.dumps({"schema": "les.light-launcher.v1", "phase": "ready", "instance_id": "one", "api_url": "http://127.0.0.1:64020"}), encoding="utf-8")
    assert server._running_api() == "http://127.0.0.1:64020"
    status.write_text(json.dumps({"schema": "les.light-launcher.v1", "phase": "ready", "instance_id": "one", "api_url": "http://external.example:80"}), encoding="utf-8")
    with pytest.raises(RuntimeError, match="некорректен"):
        server._running_api()


def test_search_uses_canonical_api_and_returns_provenance(monkeypatch):
    calls = []
    monkeypatch.setattr(server, "_running_api", lambda: "http://127.0.0.1:64020")

    def fake_request(method, path, **kwargs):
        calls.append((method, path, kwargs))
        if path.endswith("/datasets"):
            return {"datasets": [{"id": "user-1", "name": "Проект", "document_count": 1, "chunk_count": 1, "dataset_scope": "user"}, {"id": "internal", "name": "Service", "dataset_scope": "system"}]}
        return {"chunks": [{"rank": 1, "doc_id": "doc-1", "doc_name": "План.pdf", "content": "Текст", "metadata": {"dataset_id": "user-1", "page": 3, "source_path": "private/path"}, "context": {"content": "Контекст"}}], "retrieval_trace": {"fusion": "qdrant_rrf", "status": "ok"}}

    monkeypatch.setattr(server, "_request", fake_request)
    result = server.search_sources("вопрос", ["user-1"])
    assert calls[-1][1] == "/api/search"
    assert calls[-1][2]["body"]["include_context"] is True
    assert result["hits"][0]["original_url"].endswith("/api/documents/by-id/doc-1/viewer?dataset_id=user-1&doc_name=%D0%9F%D0%BB%D0%B0%D0%BD.pdf")
    assert "private/path" not in str(result)
    with pytest.raises(ValueError, match="Набор не найден"):
        server.search_sources("вопрос", ["internal"])


def test_only_read_tools_are_registered():
    pytest.importorskip("mcp")
    tools = asyncio.run(server.build_server().list_tools())
    assert {tool.name for tool in tools} == {"list_datasets", "search_sources"}
    assert all(tool.annotations.readOnlyHint is True for tool in tools)


def test_blocked_retrieval_is_not_reported_as_empty_success(monkeypatch):
    monkeypatch.setattr(server, "list_datasets", lambda: {"datasets": [{"id": "user-1", "declared_chunks": 2}]})
    monkeypatch.setattr(server, "_request", lambda *_args, **_kwargs: {
        "chunks": [], "retrieval_trace": {"status": "blocked", "error_code": "native_rrf_failed"}
    })
    monkeypatch.setattr(server, "_running_api", lambda: "http://127.0.0.1:64020")
    result = server.search_sources("вопрос", ["user-1"])
    assert result["count"] == 0
    assert result["retrieval"]["status"] == "blocked"
    assert "Qdrant" in result["retrieval"]["message"]


def test_unindexed_dataset_explains_why_search_cannot_start(monkeypatch):
    monkeypatch.setattr(server, "list_datasets", lambda: {"datasets": [{"id": "user-1", "declared_chunks": 0}]})
    monkeypatch.setattr(server, "_request", lambda *_args, **_kwargs: pytest.fail("Search must not run without fragments"))
    result = server.search_sources("вопрос", ["user-1"])
    assert result["retrieval"]["status"] == "not_ready"
    assert "нет поисковых фрагментов" in result["retrieval"]["message"]
