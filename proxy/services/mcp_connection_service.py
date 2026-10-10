"""User-selected HTTP/stdio MCP tools; no implicit server instructions or actions."""
from __future__ import annotations

import asyncio
from contextlib import contextmanager
from contextlib import AsyncExitStack
from datetime import timedelta
import hashlib
import json
import os
import sqlite3
from pathlib import Path
from urllib.parse import urlsplit
from uuid import uuid4

import httpx

from backend.rag_config import rag_meta_db_path


@contextmanager
def _db():
    path = Path(rag_meta_db_path())
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, timeout=5)
    conn.row_factory = sqlite3.Row
    try:
        conn.execute("CREATE TABLE IF NOT EXISTS les_mcp_connections (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, tools TEXT NOT NULL)")
        conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS les_mcp_unique_url ON les_mcp_connections(url)")
        if "config" not in {row[1] for row in conn.execute("PRAGMA table_info(les_mcp_connections)")}:
            conn.execute("ALTER TABLE les_mcp_connections ADD COLUMN config TEXT NOT NULL DEFAULT '{}'")
        with conn:
            yield conn
    finally:
        conn.close()


def connections() -> list[dict]:
    with _db() as conn:
        return [{"id": row["id"], "name": row["name"], "url": row["url"],
                 "tools": json.loads(row["tools"]), "transport": "http", **json.loads(row["config"])}
                for row in conn.execute("SELECT * FROM les_mcp_connections ORDER BY name,id")]


def save(name: str, url: str = "", *, transport: str = "http", command: str = "", args: list[str] | None = None) -> dict:
    name, url = name.strip(), url.strip()
    if not name or len(name) > 120:
        raise ValueError("Введите название подключения длиной до 120 символов")
    config = {"transport": transport}
    if transport == "stdio":
        program = Path(command.strip())
        arguments = list(args or [])
        if not program.is_absolute() or not program.is_file() or (os.name == "nt" and program.suffix.lower() != ".exe"):
            raise ValueError("Укажите полный путь к существующей программе MCP (.exe). Командная строка оболочки не поддерживается")
        if len(arguments) > 32 or any(not isinstance(arg, str) or len(arg) > 4096 or '\x00' in arg for arg in arguments):
            raise ValueError("Допустимо до 32 аргументов, каждый до 4096 символов")
        config.update(command=str(program.absolute()), args=arguments)
        url = "stdio:" + hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()
    elif transport == "http":
        try:
            parsed = urlsplit(url)
            valid = parsed.scheme in {"http", "https"} and parsed.hostname and not (parsed.username or parsed.password or parsed.query or parsed.fragment)
            parsed.port
        except ValueError:
            valid = False
        if len(url) > 2048 or not valid:
            raise ValueError("Укажите адрес HTTP/HTTPS без пароля, параметров и фрагмента, например http://localhost:8000/mcp")
        if parsed.scheme == "http" and parsed.hostname not in {"localhost", "127.0.0.1", "::1"}:
            raise ValueError("Для удалённого сервера используйте HTTPS")
    else:
        raise ValueError("Выберите HTTP или локальную программу")
    item = {"id": uuid4().hex, "name": name, "url": url, "tools": [], **config}
    with _db() as conn:
        if conn.execute("SELECT COUNT(*) FROM les_mcp_connections").fetchone()[0] >= 32:
            raise ValueError("Можно добавить до 32 подключений MCP")
        if conn.execute("SELECT 1 FROM les_mcp_connections WHERE url=?", (url,)).fetchone():
            raise ValueError("Этот адрес уже добавлен")
        try:
            conn.execute("INSERT INTO les_mcp_connections (id,name,url,tools,config) VALUES (?,?,?,?,?)", (item["id"], name, url, "[]", json.dumps(config)))
        except sqlite3.IntegrityError as error:
            raise ValueError("Этот адрес уже добавлен") from error
    return item


def remove(connection_id: str) -> None:
    with _db() as conn:
        conn.execute("DELETE FROM les_mcp_connections WHERE id=?", (connection_id,))


def _connection(connection_id: str) -> dict:
    item = next((item for item in connections() if item["id"] == connection_id), None)
    if item is None:
        raise ValueError("Подключение удалено. Обновите список инструментов")
    return item


class _LimitedStream(httpx.AsyncByteStream):
    def __init__(self, stream):
        self.stream = stream

    async def __aiter__(self):
        size = 0
        async for chunk in self.stream:
            size += len(chunk)
            if size > 1_000_000:
                raise ValueError("Ответ MCP превышает 1 МБ")
            yield chunk

    async def aclose(self):
        await self.stream.aclose()


class _LimitedTransport(httpx.AsyncHTTPTransport):
    async def handle_async_request(self, request):
        request.headers["Accept-Encoding"] = "identity"
        response = await super().handle_async_request(request)
        if response.headers.get("Content-Encoding", "identity").lower() != "identity":
            await response.aclose()
            raise ValueError("Сжатые ответы MCP пока не поддерживаются")
        response.stream = _LimitedStream(response.stream)
        return response


def _http_client(headers=None, timeout=None, auth=None):
    return httpx.AsyncClient(headers=headers, timeout=timeout or 15, auth=auth,
                            transport=_LimitedTransport(), follow_redirects=False, trust_env=False)


async def _exchange(connection: dict, operation):
    try:
        from mcp import ClientSession
        from mcp.client.streamable_http import streamablehttp_client
        from mcp.shared.exceptions import McpError
    except ModuleNotFoundError as error:
        raise ValueError("Компонент MCP отсутствует или повреждён. Восстановите установку LES RAG") from error

    try:
        async with asyncio.timeout(25):
            async with AsyncExitStack() as contexts:
                if connection.get("transport", "http") == "stdio":
                    from mcp import StdioServerParameters
                    from mcp.client.stdio import stdio_client
                    errlog = contexts.enter_context(open(os.devnull, "w"))
                    parameters = StdioServerParameters(command=connection["command"], args=connection.get("args", []))
                    read, write = await contexts.enter_async_context(stdio_client(parameters, errlog=errlog))
                else:
                    read, write, _ = await contexts.enter_async_context(streamablehttp_client(connection["url"], timeout=timedelta(seconds=15),
                                            sse_read_timeout=timedelta(seconds=20),
                                            httpx_client_factory=_http_client))
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    return await operation(session)
    except (ExceptionGroup, httpx.HTTPError, TimeoutError, McpError, OSError) as error:
        # Network boundaries may contain nested SDK exceptions; never expose URLs or credentials.
        raise ValueError("Сервер MCP не ответил корректно. Проверьте адрес, доступ и состояние сервера") from error


async def discover(connection_id: str) -> list[dict]:
    item = _connection(connection_id)

    async def listing(session):
        result, cursor, cursors = [], None, set()
        for _ in range(8):
            page = await session.list_tools(cursor=cursor)
            for tool in page.tools:
                data = tool.model_dump(by_alias=True, exclude_none=True)
                if len(json.dumps(data)) > 32_000:
                    raise ValueError("Описание инструмента MCP слишком большое")
                result.append(data)
                if len(result) > 128:
                    raise ValueError("Сервер вернул больше 128 инструментов")
            cursor = page.nextCursor
            if not cursor:
                if len({tool["name"] for tool in result}) != len(result):
                    raise ValueError("Сервер вернул повторяющиеся имена инструментов")
                return result
            if cursor in cursors:
                raise ValueError("Сервер повторяет страницу списка инструментов")
            cursors.add(cursor)
        raise ValueError("Слишком много страниц инструментов MCP")

    return await _exchange(item, listing)


async def enable(connection_id: str, names: list[str]) -> dict:
    if len(names) > 32 or len(names) != len(set(names)):
        raise ValueError("Выберите до 32 разных инструментов")
    found = await discover(connection_id) if names else []
    selected = [tool for tool in found if tool["name"] in names]
    if len(selected) != len(names):
        raise ValueError("Список инструментов изменился. Проверьте подключение ещё раз")
    if any(tool.get("annotations", {}).get("readOnlyHint") is not True for tool in selected):
        raise ValueError("Пока доступны только инструменты, объявленные сервером как чтение")
    from jsonschema import Draft202012Validator, SchemaError
    for tool in selected:
        schema = tool.get("inputSchema") or {}
        try:
            Draft202012Validator.check_schema(schema)
        except SchemaError as error:
            raise ValueError("Сервер передал некорректное описание аргументов инструмента") from error
        if schema.get("type") != "object":
            raise ValueError("Аргументы инструмента должны быть объектом")
        pending = [schema]
        while pending:
            node = pending.pop()
            if isinstance(node, dict):
                if any(key in node and not str(node[key]).startswith("#") for key in ("$ref", "$dynamicRef")):
                    raise ValueError("Внешние ссылки в описании аргументов не поддерживаются")
                pending.extend(node.values())
            elif isinstance(node, list):
                pending.extend(node)
    with _db() as conn:
        if conn.execute("UPDATE les_mcp_connections SET tools=? WHERE id=?", (json.dumps(selected, ensure_ascii=False), connection_id)).rowcount != 1:
            raise ValueError("Подключение удалено")
    return _connection(connection_id)


def tool_name(connection_id: str, remote_name: str) -> str:
    return "mcp_" + hashlib.sha256((connection_id + ":" + remote_name).encode()).hexdigest()[:32]


def register_tools(registry) -> None:
    from proxy.services.tool_contract_service import ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy
    from proxy.services.tool_registry_service import ToolRegistration

    for item in connections():
        for tool in item["tools"]:
            name = tool_name(item["id"], tool["name"])
            contract = ToolContract(
                name=name, version="1.0.0", title=f"{item['name']} · {tool.get('title') or tool['name']}",
                category="mcp", summary=tool.get("description") or tool["name"],
                input_schema=tool["inputSchema"], result_schema="les_tool_result_v1",
                effect=EffectClass.READ, scopes=("mcp",), timeout_seconds=30,
                retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
                result_budget=ResultBudget(max_chars=16_000, max_items=32), model_owned_fields=(),
                provenance="Selected external MCP server", tags=("mcp",),
            )

            async def handler(arguments, connection_id=item["id"], expected=tool, local_name=name):
                from proxy.services.tool_harness_service import _result
                current = _connection(connection_id)
                if expected not in current["tools"]:
                    raise ValueError("Инструмент отключён. Обновите профиль")

                async def call(session):
                    response = await session.call_tool(expected["name"], arguments)
                    return response.model_dump(by_alias=True, exclude_none=True)

                response = await _exchange(current, call)
                return _result(tool=local_name, operation="mcp_call", inputs=[],
                               status="error" if response.get("isError") else "ok",
                               result=response, trace="external_mcp")

            registry.register(ToolRegistration(contract=contract, handler=handler))
