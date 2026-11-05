"""Thin runtime adapter to the mandatory Rust owner of agent files."""
import os
from pathlib import Path
from backend import native_core
from backend.rag_config import rag_meta_db_path


class AgentStateError(RuntimeError):
    def __init__(self, code, detail):
        self.code = code
        super().__init__(detail)


def state_root():
    value = os.getenv('LES_STATE_ROOT', '').strip()
    return Path(value).resolve() if value else None


def call(action, *, project_id=0, actor='user', **arguments):
    root = state_root()
    if root is None:
        raise AgentStateError('AGENT_UNAVAILABLE', 'Каталог данных Леса не подключён. Перезапустите приложение.')
    try:
        result = native_core.required('agent_files', request={
            'schema': 'les.agent-files.v1', 'state_root': str(root),
            'database': str(Path(rag_meta_db_path()).resolve()), 'action': action,
            'arguments': dict(arguments, project_id=int(project_id or 0), actor=actor),
        })
    except native_core.NativeCoreError as error:
        raise AgentStateError('AGENT_UNAVAILABLE', 'Не установлен модуль рабочих файлов. Восстановите установку Леса.') from error
    if result.get('status') != 'completed':
        raise AgentStateError(result.get('code', 'AGENT_UNAVAILABLE'), result.get('detail', 'Не удалось открыть память Леса.'))
    return result['value']


def context_records(project_id):
    """Rust prepares advisory records; the context governor owns the final budget."""
    if state_root() is None:
        return []
    return call('projection', project_id=project_id)['records']
