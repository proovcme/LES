"""Authenticated project workspace session API."""
from __future__ import annotations

import asyncio
import sqlite3
from uuid import UUID
from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, ConfigDict, Field

from proxy.security import require_user
from proxy.services import chat_session_service
from proxy.services import chat_draft_service

router = APIRouter(prefix='/api/workspace', tags=['workspace'])


class SessionCreate(BaseModel):
    model_config = ConfigDict(extra='forbid')
    session_id: UUID | None = None
    project_id: int | None = Field(default=None, gt=0)
    title: str = Field(default='', max_length=200)


class SessionUpdate(BaseModel):
    model_config = ConfigDict(extra='forbid')
    title: str | None = Field(default=None, max_length=200)
    scope: dict | None = None
    role: str | None = Field(default=None, min_length=1, max_length=64)


class DraftAttachment(BaseModel):
    model_config = ConfigDict(extra='forbid')
    attachment_id: str = Field(pattern=r'^read_[0-9a-f]{12}$')
    name: str = Field(min_length=1, max_length=500)


class DraftUpdate(BaseModel):
    model_config = ConfigDict(extra='forbid')
    revision: int = Field(ge=0)
    text: str = Field(max_length=20000)
    attachment: DraftAttachment | None = None


async def _call(function, *args, **kwargs):
    try:
        return await asyncio.to_thread(function, *args, **kwargs)
    except chat_session_service.SessionConflictError as exc:
        raise HTTPException(409, str(exc)) from exc
    except LookupError as exc:
        raise HTTPException(404, str(exc)) from exc
    except ValueError as exc:
        raise HTTPException(422, str(exc)) from exc
    except sqlite3.OperationalError as exc:
        raise HTTPException(503, 'Не удалось записать или прочитать диалог: база временно занята или недоступна. Текст остаётся в этом окне; повторите действие.') from exc


@router.get('/sessions')
async def sessions_list(project_id: int | None = Query(default=None, gt=0), _user=Depends(require_user)):
    return {'sessions': await _call(chat_session_service.list_sessions, project_id)}


@router.post('/sessions')
async def sessions_create(req: SessionCreate, _user=Depends(require_user)):
    return await _call(chat_session_service.create_session, **req.model_dump())


@router.get('/sessions/{session_id}')
async def sessions_get(session_id: str, _user=Depends(require_user)):
    session = await _call(chat_session_service.get_session, session_id)
    if session is None:
        raise HTTPException(404, 'Session not found')
    return session


@router.patch('/sessions/{session_id}')
async def sessions_update(session_id: str, req: SessionUpdate, _user=Depends(require_user)):
    return await _call(chat_session_service.update_session, session_id, **req.model_dump(exclude_unset=True))


@router.get('/sessions/{session_id}/draft')
async def draft_get(session_id: str, _user=Depends(require_user)):
    return await _call(chat_draft_service.get_draft, session_id)


@router.put('/sessions/{session_id}/draft')
async def draft_put(session_id: str, req: DraftUpdate, _user=Depends(require_user)):
    return await _call(chat_draft_service.save_draft, session_id, **req.model_dump())
