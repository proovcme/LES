"""Direct reranker route."""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Any

from fastapi import APIRouter, Depends, HTTPException
from pydantic import BaseModel, Field

from proxy.security import require_admin, require_user
from proxy.services.resource_governor import chat_generation_allowed

try:
    from backend.reranker import Reranker, select_reranker_cls

    RERANKER_AVAILABLE = True
except ImportError:
    Reranker = None
    select_reranker_cls = None
    RERANKER_AVAILABLE = False

router = APIRouter(prefix="/api", tags=["rerank"])


@dataclass
class RerankRouterState:
    llm_semaphore: Any
    current_mode: dict[str, Any] | None = None


_state: RerankRouterState | None = None


@router.get("/rerank/status")
async def rerank_status(_user=Depends(require_user)):
    from backend.local_reranker import readiness
    return readiness()


class RerankChunk(BaseModel):
    text: str = Field(max_length=8000)
    score: float = Field(default=0, allow_inf_nan=False)
    metadata: dict[str, Any] = Field(default_factory=dict)


class RerankRequest(BaseModel):
    query: str = Field(min_length=1, max_length=4000)
    chunks: list[RerankChunk] = Field(min_length=1, max_length=64)
    top_k: int = Field(default=5, ge=1, le=64)


def set_rerank_state(state: RerankRouterState) -> None:
    global _state
    _state = state


@router.post("/rerank")
async def rerank_direct(request: RerankRequest, _admin=Depends(require_admin)):
    """
    Direct reranker call.
    Body: {"query": str, "chunks": [{"text": str, "score": float, "metadata": dict}], "top_k": int}
    """
    if not RERANKER_AVAILABLE:
        raise HTTPException(503, "reranker недоступен")
    query = request.query.strip()
    chunks = [chunk.model_dump() for chunk in request.chunks]
    top_k = request.top_k

    if not query or not chunks:
        raise HTTPException(400, "query и chunks обязательны")

    state = _state
    if state is not None:
        allowed, resource_reason = chat_generation_allowed(state.current_mode)
        if not allowed:
            raise HTTPException(status_code=409, detail=resource_reason)

    mlx_url = os.getenv("MLX_URL", "http://127.0.0.1:8080")
    reranker_cls = select_reranker_cls()
    reranker = reranker_cls(mlx_url=mlx_url)
    if state is None:
        ranked = await reranker.rerank(query, chunks, top_k=top_k)
    else:
        async with state.llm_semaphore:
            ranked = await reranker.rerank(query, chunks, top_k=top_k)

    return {
        "ranked": [
            {
                "text": r.text,
                "score": r.score,
                "original_score": r.original_score,
                "rank": r.rank,
                "metadata": r.metadata,
            }
            for r in ranked
        ]
    }
