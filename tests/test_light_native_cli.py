"""Installed CLI and stdio protocol against an isolated loopback LES API."""
import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import threading
import time
from urllib.parse import parse_qs, urlsplit

import pytest


@pytest.fixture
def les(tmp_path):
    configured = os.getenv("LES_TEST_NATIVE_RUNTIME")
    if not configured:
        pytest.skip("Build the Rust runtime to verify the native CLI")
    binary = Path(configured).resolve(strict=True)
    contract = subprocess.run([str(binary), "--contract"], check=True, capture_output=True)
    assert json.loads(contract.stdout)["native_cli"]
    calls = []
    identity = ["fixture"]

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def send_json(self, value, status=200, headers=None):
            raw = json.dumps(value, ensure_ascii=False).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            for name, header in (headers or {}).items():
                self.send_header(name, header)
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            calls.append(("GET", self.path, self.headers.get("X-API-Key"), None))
            if self.path == "/api/light/instance":
                self.send_json({"instance_id": identity[0]})
            elif self.path == "/api/status":
                self.send_json({"status": "ready"})
            elif self.path == "/api/projects":
                self.send_json({"projects": []})
            elif self.path in ("/api/documents/datasets?limit=1000", "/api/light/library"):
                self.send_json({"datasets": [
                    {"id": "user-1", "display_name": "Данные 日本語", "document_count": 1, "chunk_count": 3},
                    {"id": "user-2", "name": "Пустой"},
                    {"id": "hidden", "dataset_scope": "system", "chunk_count": 12},
                ]})
            elif self.path.startswith("/api/light/library?"):
                query = parse_qs(urlsplit(self.path).query)
                if query.get("dataset_id") == ["user-1"]:
                    self.send_json({"dataset_id":"user-1","documents":[
                        {"id":"doc/id","dataset_id":"user-1","file_name":"Папка/План #1.pdf"},
                    ]})
                else:
                    self.send_json({},status=404)
            elif self.path.startswith("/api/light/agent-workspace?"):
                self.send_json({"revision":"missing","text":""})
            else:
                self.send_json({}, status=404)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            calls.append(("POST", self.path, self.headers.get("X-API-Key"), body))
            if self.path == "/api/workspace/sessions":
                self.send_json({"session_id": "new-chat"})
            elif self.path == "/api/chat":
                self.send_json({"answer": "Ответ"})
            elif self.path == "/api/rag/datasets":
                self.send_json({"id":"created","name":body["name"]})
            elif self.path == "/api/rag/index-external":
                self.send_json({"status":"started","intake_job_id":"test-intake"})
            elif self.path == "/api/light/agent-workspace":
                self.send_json({"persisted":True,"revision":"fixture-revision"})
            elif self.path == "/api/search":
                trace = {"status": "blocked", "error_code": "native_rrf_failed"}
                if body["query"] == "сбой":
                    self.send_json({"retrieval_trace": trace})
                    return
                self.send_json({"chunks": [{
                    "rank": 1, "doc_id": "doc/id", "doc_name": "План #1.pdf", "content": "Строка",
                    "metadata": {"dataset_id": "user-1", "page": 3, "source_path": "private/path"},
                    "context": {"content": "Раздел", "metadata": {"context_fragments": [{
                        "content": "Соседняя страница", "metadata": {"page": 4, "qdrant_point_id": "next"},
                    }]}},
                }], "retrieval_trace": {"status": "ok", "fusion": "qdrant_rrf"}})
            else:
                self.send_json({}, status=404)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    state = tmp_path / "Лес с пробелами"
    state.mkdir()
    marker = state / "launcher-status.json"
    marker.write_text(json.dumps({
        "schema": "les.light-launcher.v1", "phase": "ready", "instance_id": "fixture",
        "api_url": f"http://127.0.0.1:{server.server_port}",
    }), encoding="utf-8")
    env = os.environ | {"LES_LIGHT_STATE": str(state), "LES_LIGHT_MCP_API_KEY": "test-only-key"}
    try:
        yield binary, env, calls, identity, marker
    finally:
        server.shutdown()
        thread.join(timeout=5)
        server.server_close()


def command(les, *args, check=True, data=None):
    binary, env, *_ = les
    return subprocess.run([str(binary), "--cli", *args], env=env, check=check,
                          input=data, capture_output=True, encoding="utf-8", timeout=10)


def test_cli_tools_and_lifecycle_contract(les):
    _, env, calls, _, marker = les
    state = Path(env['LES_LIGHT_STATE'])
    tools = json.loads(command(les, 'tools').stdout)['tools']
    assert 'create_dataset' in {tool['name'] for tool in tools}
    result = command(les, 'tool', 'create_dataset', data='{"name":"Набор 🌲"}\n')
    assert json.loads(result.stdout) == {'id':'created','name':'Набор 🌲'}
    assert calls[-1][3] == {'name':'Набор 🌲'}
    assert command(les, 'tool', 'create_dataset', data='[]', check=False).returncode != 0
    assert 'без настольного управления' in command(les, 'stop', check=False).stderr
    supervisor = state/'supervisor-status.json'
    supervisor.write_text(json.dumps({'phase':'running','control':1,'launch_id':'42-100'}))
    stop_marker = state/'desktop-stop-42-100.request'
    saw_stop = threading.Event()

    def owner():
        deadline = time.monotonic() + 5
        while not stop_marker.is_file() and time.monotonic() < deadline:
            time.sleep(.02)
        if stop_marker.is_file():
            stop_marker.unlink()
            saw_stop.set()
            supervisor.write_text(json.dumps({'phase':'stopped','launch_id':'42-100'}))
    worker = threading.Thread(target=owner, daemon=True)
    worker.start()
    assert json.loads(command(les, 'stop').stdout) == {'status':'stopped'}
    worker.join(timeout=5)
    assert saw_stop.is_set() and not stop_marker.exists()
    marker.unlink()
    assert json.loads(command(les, 'status').stdout)['status'] == 'stopped'
    assert json.loads(command(les, 'stop').stdout)['already_stopped']


def test_cli_uses_existing_session_and_creates_new_session(les):
    assert json.loads(command(les, "status").stdout)["status"] == "ready"
    assert json.loads(command(les, "projects").stdout)["projects"] == []
    assert json.loads(command(les, "ask", "Что в 日本語?", "--chat", "saved", "--dataset=user-1").stdout) == {
        "answer": "Ответ", "session_id": "saved",
    }
    assert json.loads(command(les, "ask", "Новый вопрос").stdout)["session_id"] == "new-chat"
    calls = les[2]
    assert [item[3] for item in calls if item[1] == "/api/chat"] == [
        {"question": "Что в 日本語?", "session_id": "saved", "mode": "search", "dataset_ids": ["user-1"]},
        {"question": "Новый вопрос", "session_id": "new-chat", "mode": "agent", "dataset_ids": []},
    ]
    assert all(key is None if path == "/api/light/instance" else key == "test-only-key"
               for _, path, key, _ in calls)


def test_cli_search_provenance_scope_and_restart_guard(les):
    result = json.loads(command(les, "search", "Где файл #1 日本語?").stdout)
    hit = result["hits"][0]
    assert hit["page"] == 3 and hit["context_fragments"][0]["page"] == 4
    parsed = urlsplit(hit["original_url"])
    assert parsed.path == "/api/documents/by-id/doc%2Fid/viewer"
    assert parse_qs(parsed.query) == {"dataset_id": ["user-1"], "doc_name": ["План #1.pdf"]}
    assert "private/path" not in str(result)
    calls = les[2]
    assert calls[-1][3]["dataset_ids"] == ["user-1", "user-2"]
    assert calls[-1][3]["include_context"] is True
    assert command(les, "search", "вопрос", "--dataset", "hidden", check=False).returncode == 1
    assert json.loads(command(les, "search", "вопрос", "--dataset", "user-2").stdout)["retrieval"]["status"] == "not_ready"
    assert "Qdrant" in json.loads(command(les, "search", "сбой").stdout)["retrieval"]["message"]
    les[3][0] = "different-process"
    before = len(calls)
    assert "Экземпляр LES RAG изменился" in command(les, "status", check=False).stderr
    assert [item[1] for item in calls[before:]] == ["/api/light/instance"]


def test_stdio_mcp_interoperates_with_existing_client(les):
    mcp = pytest.importorskip("mcp")
    from mcp.client.stdio import stdio_client
    binary, env, *_ = les

    async def run():
        params = mcp.StdioServerParameters(command=str(binary), args=["--cli", "mcp"], env=env)
        async with stdio_client(params) as (read, write):
            async with mcp.ClientSession(read, write) as session:
                await session.initialize()
                tools = (await session.list_tools()).tools
                flags = {tool.name:tool.annotations for tool in tools}
                assert flags['search_sources'].readOnlyHint
                assert not flags['create_dataset'].readOnlyHint
                assert flags['workspace_write'].destructiveHint
                result = await session.call_tool("list_datasets", {})
                assert not result.isError
                assert [row["id"] for row in result.structuredContent["datasets"]] == ["user-1", "user-2"]
                result = await session.call_tool("search_sources", {"query": "Где план?", "dataset_ids": ["user-1"]})
                assert not result.isError
                assert result.structuredContent["hits"][0]["document"] == "План #1.pdf"
                rejected = await session.call_tool("search_sources", {"query": "Где план?", "dataset_ids": ["hidden"]})
                assert rejected.isError and "Набор не найден" in rejected.content[0].text
                created = await session.call_tool("create_dataset", {"name":"Данные 🌲"})
                assert created.structuredContent == {"id":"created","name":"Данные 🌲"}
                docs = await session.call_tool("list_documents", {"dataset_id":"user-1"})
                assert docs.structuredContent['documents'][0]['original_url'].endswith(
                    'dataset_id=user-1&doc_name=%D0%9F%D0%B0%D0%BF%D0%BA%D0%B0%2F%D0%9F%D0%BB%D0%B0%D0%BD+%231.pdf')
                path = str(Path(env['LES_LIGHT_STATE']) / 'источники')
                attached = await session.call_tool("attach_folder", {"dataset_id":"user-1","path":path})
                assert attached.structuredContent['intake_job_id'] == 'test-intake'
                _, _, _, request = les[2][-1]
                assert request == {"path":path,"dataset_id":"user-1","parse":True,"background":True}
                blocked = await session.call_tool("attach_folder", {"dataset_id":"hidden","path":path})
                assert blocked.isError and les[2][-1][0] == 'GET'
                read = await session.call_tool("workspace_read", {"action":"read","area":"files","path":"Тест.md"})
                written = await session.call_tool("workspace_write", {
                    "action":"save","area":"files","path":"Тест.md","text":"日本語",
                    "expected_revision":read.structuredContent["revision"],
                })
                assert written.structuredContent["persisted"]
                answer = await session.call_tool("les_chat", {"question":"Проверь память", "project_id":4})
                assert answer.structuredContent == {"answer":"Ответ","session_id":"new-chat"}
                assert les[2][-1][3]["mode"] == "agent" and les[2][-1][3]["project_id"] == 4

    asyncio.run(asyncio.wait_for(run(), timeout=20))
