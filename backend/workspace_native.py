"""Compatibility bridge; SQLite session/draft decisions belong to the native owner."""
import logging
from pathlib import Path
import sqlite3

from backend import native_core

logger = logging.getLogger(__name__)


class WorkspaceUnavailable(sqlite3.OperationalError):
    """The mandatory native owner is missing or incompatible, not an empty catalog."""


def call(database, action, **arguments):
    return invoke('workspace', 'les.workspace.v1', database, action, arguments)


def catalog_call(database, action, **arguments):
    return invoke('catalog', 'les.catalog.v1', database, action, arguments)


def invoke(operation, schema, database, action, arguments):
    try:
        result = native_core.required(operation, request={
            'schema': schema, 'database': str(Path(database).resolve()),
            'action': action, 'arguments': arguments,
        })
    except native_core.NativeCoreError as error:
        logger.error('Native workspace unavailable: code=%s', str(error))
        raise WorkspaceUnavailable('Не установлен совместимый модуль хранения данных. Восстановите установку Леса.') from error
    if result.get('status') == 'completed' and 'value' in result:
        return result['value']
    code = result.get('code')
    logger.warning('Native workspace operation failed: action=%s code=%s sqlite_extended=%s',
                   action, code, result.get('sqlite_extended'))
    detail = str(result.get('detail') or 'Не удалось прочитать или сохранить диалог.')
    if code == 'WORKSPACE_CONFLICT':
        from proxy.services.chat_session_service import SessionConflictError
        raise SessionConflictError(detail)
    if code == 'WORKSPACE_NOT_FOUND':
        raise LookupError(detail)
    if code == 'WORKSPACE_INVALID':
        raise ValueError(detail)
    raise sqlite3.OperationalError(detail)
