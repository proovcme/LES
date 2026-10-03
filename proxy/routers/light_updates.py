"""Light deliberately has no full-LES /patch or /mac installation routes."""
from fastapi import APIRouter, Depends, HTTPException
from proxy.security import require_admin, require_user
from proxy.services import light_update_service as service

router = APIRouter(prefix="/api/update", tags=["light-update"])


@router.get("/check")
async def check(_user=Depends(require_user)):
    try:
        return await service.check_update()
    except service.LightUpdateError as error:
        raise HTTPException(502, str(error)) from error


@router.get("/status")
async def status(_user=Depends(require_user)):
    return service.read_status()


@router.post("/install")
async def install(_admin=Depends(require_admin)):
    try:
        return await service.install_update()
    except service.LightUpdateError as error:
        raise HTTPException(409, str(error)) from error
