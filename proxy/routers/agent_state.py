"""Local agent state API; paths and transaction rules belong to Rust."""
from typing import Literal
from fastapi import APIRouter, Depends, HTTPException, Query
from pydantic import BaseModel, ConfigDict, Field
from proxy.security import require_user
from proxy.services.agent_state_service import call, AgentStateError
from proxy.services.native_storage_http import storage_call

router = APIRouter(prefix='/api/workspace/agent-state', tags=['workspace'])


class AgentStateWrite(BaseModel):
    model_config = ConfigDict(extra='forbid')
    action: Literal['save', 'edit', 'delete', 'restore', 'policy_save']
    project_id: int = Field(default=0, ge=0)
    area: Literal['user', 'memory', 'files', 'policy'] = 'memory'
    path: str = Field(default='', max_length=240)
    expected_revision: str = Field(min_length=1, max_length=64)
    text: str | None = Field(default=None, max_length=1048576)
    old_text: str | None = Field(default=None, max_length=131072)
    new_text: str | None = Field(default=None, max_length=131072)
    reason: str = Field(default='', max_length=500)
    history_id: str | None = Field(default=None, max_length=80)
    side: Literal['before', 'after'] = 'after'
    value: dict | None = None


async def invoke(action, **arguments):
    try:
        return await storage_call(call, action, **arguments)
    except AgentStateError as error:
        status = 409 if error.code == 'AGENT_CONFLICT' else 404 if error.code == 'AGENT_NOT_FOUND' else 403 if error.code == 'AGENT_DISABLED' else 422 if error.code in {'AGENT_INVALID', 'AGENT_PATH_UNSAFE', 'AGENT_TOO_LARGE', 'AGENT_ENCODING'} else 503
        raise HTTPException(status, str(error)) from error


@router.get('')
async def get_state(action: Literal['read', 'bundle', 'list', 'history', 'version', 'policy', 'export'] = 'bundle',
                    project_id: int = Query(default=0, ge=0), area: Literal['user', 'memory', 'files', 'policy'] = 'memory',
                    path: str = Query(default='', max_length=240), history_id: str = Query(default='', max_length=80),
                    offset: int = Query(default=0, ge=0),
                    include_deleted: bool = False,
                    expected_revision: str = Query(default='', max_length=64),
                    _user=Depends(require_user)):
    return await invoke(action, project_id=project_id, area=area, path=path, history_id=history_id, offset=offset, include_deleted=include_deleted, expected_revision=expected_revision)


@router.post('')
async def write_state(req: AgentStateWrite, _user=Depends(require_user)):
    arguments = req.model_dump(exclude_none=True)
    action = arguments.pop('action')
    return await invoke(action, **arguments)
