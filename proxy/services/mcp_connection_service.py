"""Compatibility codec for the Rust MCP catalog and HTTP/stdio clients."""
from __future__ import annotations
import asyncio
import hashlib
import os
from pathlib import Path

from backend import native_core
from backend.rag_config import rag_meta_db_path


def _native(action, **fields):
    program = Path(os.getenv('LES_NATIVE_MCP_WORKER') or
        str(Path(__file__).resolve().parents[2] / ('native/api/les-api.exe' if os.name == 'nt' else 'native/api/les-api')))
    if not program.is_absolute() or not program.is_file():
        raise ValueError('Компонент MCP отсутствует или повреждён. Восстановите установку LES RAG')
    database = Path(rag_meta_db_path()).resolve()
    database.parent.mkdir(parents=True, exist_ok=True)
    result = native_core.required('worker_mcp', request={
        'schema': 'les.worker-host.v1', 'program': str(program), 'args': ['--serve-mcp'],
        'directory': str(Path.cwd()), 'timeout_ms': 30000,
        'context': {'database': str(database)},
        'input': {'schema': 'les.mcp.v1', 'action': action, 'database': str(database), 'working_directory': str(Path.cwd()), **fields}})
    if result.get('status') != 'completed':
        raise ValueError('Сервер MCP не ответил корректно. Проверьте адрес, доступ и состояние сервера')
    response = (result.get('result') or {}).get('result') or {}
    if response.get('ok') is not True:
        raise ValueError(response.get('error', {}).get('message', 'Не удалось выполнить запрос MCP'))
    return response['value']


def connections():
    return _native('list')['connections']


def save(name, url='', *, transport='http', command='', args=None):
    return _native('save', name=name, url=url, transport=transport, command=command, args=list(args or []))


def remove(connection_id):
    _native('remove', connection_id=connection_id)


def _connection(connection_id):
    item = next((item for item in connections() if item['id'] == connection_id), None)
    if item is None:
        raise ValueError('Подключение удалено. Обновите список инструментов')
    return item


async def discover(connection_id):
    return (await asyncio.to_thread(_native, 'discover', connection_id=connection_id))['tools']


async def enable(connection_id, names):
    return await asyncio.to_thread(_native, 'enable', connection_id=connection_id, names=list(names))


def tool_name(connection_id, remote_name):
    return 'mcp_' + hashlib.sha256((connection_id + ':' + remote_name).encode()).hexdigest()[:32]


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

                response = await asyncio.to_thread(_native, 'call', connection_id=connection_id,
                                                   expected=expected, arguments=arguments)
                return _result(tool=local_name, operation="mcp_call", inputs=[],
                               status="error" if response.get("isError") else "ok",
                               result=response, trace="external_mcp")

            registry.register(ToolRegistration(contract=contract, handler=handler))
