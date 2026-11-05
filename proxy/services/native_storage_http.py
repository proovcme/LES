"""Human-readable storage failures shared by transitional Python HTTP routes."""
import asyncio
import sqlite3
from fastapi import HTTPException
from backend.workspace_native import WorkspaceUnavailable


async def storage_call(function, *args, **kwargs):
    try:
        return await asyncio.to_thread(function, *args, **kwargs)
    except WorkspaceUnavailable as error:
        raise HTTPException(503, str(error)) from error
    except sqlite3.OperationalError as error:
        raise HTTPException(503, 'Не удалось прочитать или сохранить данные: хранилище занято или недоступно. Повторите действие.') from error
