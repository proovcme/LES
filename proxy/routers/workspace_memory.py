"""Local-user controls for advisory notes and inspectable conversation context."""
from __future__ import annotations

import asyncio

from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, Field, field_validator

from proxy.security import require_user
from proxy.services import memory_service, project_service

router = APIRouter(prefix='/api/workspace/memory', tags=['workspace'])


class ContextUpdate(BaseModel):
    expected_revision: int = Field(ge=0)
    summary: str | None = Field(default=None, max_length=2200)
    enabled: bool | None = None
    auto_summary: bool | None = None


class ContextAction(BaseModel):
    expected_revision: int = Field(ge=0)


async def _context_session(session_id):
    from proxy.services.chat_session_service import get_session
    if await asyncio.to_thread(get_session, session_id) is None:
        raise HTTPException(404, 'Чат не найден')


@router.get('/context/{session_id}')
async def get_conversation_context(session_id: str, _user=Depends(require_user)):
    from proxy.services.conversation_context_service import context_status
    await _context_session(session_id)
    return await asyncio.to_thread(context_status, session_id)


@router.patch('/context/{session_id}')
async def edit_conversation_context(session_id: str, req: ContextUpdate, _user=Depends(require_user)):
    from proxy.services.conversation_context_service import update_context, ContextConflict
    await _context_session(session_id)
    changes = req.model_dump(exclude_none=True)
    if 'summary' in changes:
        changes['summary'] = changes['summary'].strip()
        changes['last_error'] = ''
        changes['model_revision'] = 'user_edit'
    try:
        return await asyncio.to_thread(update_context, session_id, **changes)
    except ContextConflict as error:
        raise HTTPException(409, str(error)) from error


@router.post('/context/{session_id}/forget')
async def forget_conversation_context(session_id: str, req: ContextAction, _user=Depends(require_user)):
    from proxy.services.conversation_context_service import forget_context, ContextConflict
    await _context_session(session_id)
    try:
        return await asyncio.to_thread(forget_context, session_id, expected_revision=req.expected_revision)
    except ContextConflict as error:
        raise HTTPException(409, str(error)) from error


@router.post('/context/{session_id}/summarize')
async def summarize_conversation_context(session_id: str, req: ContextAction, _user=Depends(require_user)):
    from proxy.services.conversation_context_service import summarize, ContextConflict
    from proxy.routers.chat import get_chat_state, _active_dispatcher_reindex_jobs
    from proxy.services.runtime_admission import evaluate_chat_admission, count_active_jobs
    await _context_session(session_id)
    state = get_chat_state()
    admission = evaluate_chat_admission(current_mode=state.current_mode, metrics_cache=state.metrics_cache,
        active_jobs=count_active_jobs(state.job_service, state.job_tracker) + _active_dispatcher_reindex_jobs(state))
    if not admission.allowed:
        raise HTTPException(503, 'Сводка подождёт: сейчас не хватает ресурсов или обрабатываются документы. История сохранена.')
    try:
        return await summarize(session_id, force=True, expected_revision=req.expected_revision)
    except ContextConflict as error:
        raise HTTPException(409, str(error)) from error


class NoteCreate(BaseModel):
    text: str = Field(max_length=2000)
    project_id: int = Field(default=0, ge=0)
    source_session_id: str | None = Field(default=None, min_length=1, max_length=200)

    @field_validator('text')
    @classmethod
    def validate_text(cls, value: str) -> str:
        return memory_service.validate_note_text(value)


class NoteUpdate(BaseModel):
    project_id: int = Field(ge=0)
    text: str | None = Field(default=None, max_length=2000)
    enabled: bool | None = None

    @field_validator('text')
    @classmethod
    def validate_text(cls, value: str | None) -> str | None:
        return memory_service.validate_note_text(value) if value is not None else None


async def _check_project(project_id: int) -> None:
    if project_id and await asyncio.to_thread(project_service.get_project, project_id) is None:
        raise HTTPException(404, 'Объект не найден')


@router.get('')
async def list_memory(project_id: int = Query(default=0, ge=0), _user=Depends(require_user)):
    return {'notes': await asyncio.to_thread(memory_service.list_notes, limit=500, project_id=project_id)}


@router.post('')
async def create_memory(req: NoteCreate, _user=Depends(require_user)):
    await _check_project(req.project_id)
    if req.source_session_id is not None:
        from proxy.services import chat_session_service
        session = await asyncio.to_thread(chat_session_service.get_session, req.source_session_id)
        if session is None or int(session.get('project_id') or 0) != req.project_id:
            raise HTTPException(422, 'Исходный чат должен принадлежать выбранной области памяти')
    return await asyncio.to_thread(memory_service.create_note, req.text,
                                   project_id=req.project_id, source_session_id=req.source_session_id)


@router.patch('/{note_id}')
async def update_memory(note_id: int, req: NoteUpdate, _user=Depends(require_user)):
    await _check_project(req.project_id)
    note = await asyncio.to_thread(memory_service.update_note, note_id, project_id=req.project_id,
                                   text=req.text, enabled=req.enabled)
    if note is None:
        raise HTTPException(404, 'Заметка не найдена в выбранной области')
    return note


@router.delete('/{note_id}')
async def delete_memory(note_id: int, project_id: int = Query(ge=0), _user=Depends(require_user)):
    deleted = await asyncio.to_thread(memory_service.delete_note, note_id, project_id=project_id)
    if not deleted:
        raise HTTPException(404, 'Заметка не найдена в выбранной области')
    return {'deleted': True}
