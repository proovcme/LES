"""Authenticated management of user-selected MCP connections."""
from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel, Field
from typing import Literal
from proxy.security import require_admin, require_user
from proxy.services import mcp_connection_service as service

router = APIRouter(prefix="/api/mcp", tags=["mcp"])


class ConnectionRequest(BaseModel):
    name: str = Field(min_length=1, max_length=120)
    url: str = Field(default="", max_length=2048)
    transport: Literal["http", "stdio"] = "http"
    command: str = Field(default="", max_length=4096)
    args: list[str] = Field(default_factory=list, max_length=32)


class ToolsRequest(BaseModel):
    names: list[str] = Field(default_factory=list, max_length=32)


@router.get("")
async def list_connections(_user=Depends(require_user)):
    return {"connections": service.connections()}


@router.post("")
async def add_connection(req: ConnectionRequest, _admin=Depends(require_admin)):
    try:
        return service.save(req.name, req.url, transport=req.transport, command=req.command, args=req.args)
    except ValueError as error:
        raise HTTPException(409, str(error)) from error


@router.post("/{connection_id}/probe")
async def probe(connection_id: str, _admin=Depends(require_admin)):
    try:
        return {"tools": await service.discover(connection_id)}
    except ValueError as error:
        raise HTTPException(409, str(error)) from error


@router.put("/{connection_id}/tools")
async def select_tools(connection_id: str, req: ToolsRequest, _admin=Depends(require_admin)):
    try:
        result = await service.enable(connection_id, req.names)
        from proxy.routers import tools
        tools._TRUSTED_EXECUTOR = None
        return result
    except ValueError as error:
        raise HTTPException(409, str(error)) from error


@router.delete("/{connection_id}")
async def delete_connection(connection_id: str, _admin=Depends(require_admin)):
    service.remove(connection_id)
    from proxy.routers import tools
    tools._TRUSTED_EXECUTOR = None
    return {"status": "deleted"}
