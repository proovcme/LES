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


async def discover(connection_id):
    return (await asyncio.to_thread(_native, 'discover', connection_id=connection_id))['tools']


async def enable(connection_id, names):
    return await asyncio.to_thread(_native, 'enable', connection_id=connection_id, names=list(names))


def tool_name(connection_id, remote_name):
    return 'mcp_' + hashlib.sha256((connection_id + ':' + remote_name).encode()).hexdigest()[:32]
