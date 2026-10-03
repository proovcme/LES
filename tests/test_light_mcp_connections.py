import asyncio
import socket
import threading
import time
import pytest

from proxy.services import mcp_connection_service as service


@pytest.fixture
def isolated_db(tmp_path, monkeypatch):
    monkeypatch.setattr(service, "rag_meta_db_path", lambda: tmp_path / "mcp.db")
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")


@pytest.fixture
def server():
    import uvicorn
    from mcp.server.fastmcp import FastMCP
    from mcp.types import ToolAnnotations
    app = FastMCP("Synthetic acceptance", stateless_http=True, json_response=True)
    calls = []

    @app.tool(annotations=ToolAnnotations(readOnlyHint=True))
    def echo(text: str) -> str:
        calls.append(text)
        return "Ответ: " + text

    @app.tool(annotations=ToolAnnotations(readOnlyHint=False))
    def change() -> str:
        calls.append("WRITE")
        return "Unexpected write"

    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    runtime = uvicorn.Server(uvicorn.Config(app.streamable_http_app(), log_level="critical"))
    thread = threading.Thread(target=runtime.run, kwargs={"sockets": [sock]}, daemon=True)
    thread.start()
    for _ in range(250):
        if runtime.started:
            break
        time.sleep(.02)
    assert runtime.started
    try:
        yield f"http://127.0.0.1:{port}/mcp", calls
    finally:
        runtime.should_exit = True
        thread.join(timeout=5)
        sock.close()
        assert not thread.is_alive()


def test_real_mcp_http_catalog_allowlist_execution_disable_delete(isolated_db, server):
    from proxy.services.tool_harness_service import ToolHarness
    url, calls = server
    connection = service.save("Библиотека", url)
    found = asyncio.run(service.discover(connection["id"]))
    assert {tool["name"] for tool in found} == {"echo", "change"}
    assert calls == []
    with pytest.raises(ValueError, match="чтение"):
        asyncio.run(service.enable(connection["id"], ["change"]))
    asyncio.run(service.enable(connection["id"], ["echo"]))
    from proxy.services.chat_profile_service import _factory_contracts
    assert not any(name.startswith("mcp_") for name in _factory_contracts()["agent"]["tools"])
    harness = ToolHarness()
    name = service.tool_name(connection["id"], "echo")
    shortlist = harness.shortlist("Верни текст", mode="agent", allowed_tools=[name])
    assert [tool["name"] for tool in shortlist["tools"]] == [name]
    result = asyncio.run(harness.call_async(name, {"text": "Кедр"}))
    assert result["status"] == "ok", result
    assert "Кедр" in str(result["result"])
    assert calls == ["Кедр"]
    asyncio.run(service.enable(connection["id"], []))
    result = asyncio.run(harness.call_async(name, {"text": "Не вызывать"}))
    assert result["status"] != "ok"
    assert calls == ["Кедр"]
    service.remove(connection["id"])
    assert service.connections() == []


@pytest.mark.parametrize("url", ["file:///tmp/file", "http://user:password@localhost/mcp", "http://example.org/mcp", "https://example.org/mcp?key=secret", "https://example.org/mcp#fragment"])
def test_invalid_addresses_not_persisted(isolated_db, url):
    with pytest.raises(ValueError):
        service.save("Проверка", url)
    assert service.connections() == []


def test_duplicate_and_missing_connection(isolated_db):
    service.save("Первое", "https://example.org/mcp")
    with pytest.raises(ValueError, match="уже добавлен"):
        service.save("Второе", "https://example.org/mcp")
    with pytest.raises(ValueError, match="удалено"):
        asyncio.run(service.discover("absent"))


def test_connection_refused_has_actionable_error(isolated_db):
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    try:
        item = service.save("Недоступный", f"http://127.0.0.1:{port}/mcp")
        with pytest.raises(ValueError, match="Проверьте адрес"):
            asyncio.run(service.discover(item["id"]))
    finally:
        sock.close()


def test_real_stdio_process_and_disabled_handler(isolated_db, tmp_path):
    import sys
    from proxy.services.tool_harness_service import ToolHarness
    script = tmp_path / "сервер с пробелами.py"
    script.write_text('from mcp.server.fastmcp import FastMCP\nfrom mcp.types import ToolAnnotations\nm = FastMCP("stdio acceptance")\n@m.tool(annotations=ToolAnnotations(readOnlyHint=True))\ndef echo(text: str) -> str:\n    return "Прочитано: " + text\nm.run()\n', encoding="utf-8")
    item = service.save("Локальный", transport="stdio", command=sys.executable, args=[str(script)])
    found = asyncio.run(service.discover(item["id"]))
    assert [tool["name"] for tool in found] == ["echo"]
    asyncio.run(service.enable(item["id"], ["echo"]))
    name = service.tool_name(item["id"], "echo")
    result = asyncio.run(ToolHarness().call_async(name, {"text": "Берёза"}))
    assert result["status"] == "ok", result
    assert "Берёза" in str(result["result"])
    with pytest.raises(ValueError, match="уже добавлен"):
        service.save("Дубль", transport="stdio", command=sys.executable, args=[str(script)])
    script.unlink()
    with pytest.raises(ValueError, match="Сервер MCP"):
        asyncio.run(service.discover(item["id"]))


def test_stdio_requires_existing_absolute_executable(isolated_db):
    with pytest.raises(ValueError, match="полный путь"):
        service.save("Нельзя", transport="stdio", command="python", args=["-c", "print(1)"])
    assert service.connections() == []


def test_missing_sdk_has_repair_message(isolated_db, monkeypatch):
    import sys
    monkeypatch.setitem(sys.modules, "mcp", None)
    item = service.save("Проверка", "https://example.org/mcp")
    with pytest.raises(ValueError, match="Восстановите установку"):
        asyncio.run(service.discover(item["id"]))


def test_http_connection_survives_stdio_schema_migration(tmp_path, monkeypatch):
    import sqlite3
    db = tmp_path / "old.db"
    conn = sqlite3.connect(db)
    try:
        conn.execute("CREATE TABLE les_mcp_connections (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, tools TEXT NOT NULL)")
        conn.execute("INSERT INTO les_mcp_connections VALUES ('old', 'Старое', 'https://example.org/mcp', '[]')")
        conn.commit()
    finally:
        conn.close()
    monkeypatch.setattr(service, "rag_meta_db_path", lambda: db)
    assert service.connections() == [{"id": "old", "name": "Старое", "url": "https://example.org/mcp", "tools": [], "transport": "http"}]
