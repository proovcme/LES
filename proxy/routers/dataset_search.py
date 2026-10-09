"""Dataset search endpoints."""
from __future__ import annotations
import logging
from typing import Any
from fastapi import APIRouter, Depends, HTTPException
from backend.rag_config import rag_runtime_config
from backend.reranker import select_reranker_cls
from proxy.config import mlx_url
from proxy.security import require_user
from proxy.services.context_expander_service import expand_context_windows
from proxy.services.retrieval_service import classify_query, resolve_dataset_ids, retrieve_chat_chunks

from proxy.services.dataset_contracts import (RetrievalDebugRequest, SearchRequest)

import proxy.services.dataset_runtime as dataset_runtime


logger = logging.getLogger(__name__)
router = APIRouter(prefix="/api/rag", tags=["rag"])
search_router = APIRouter(prefix="/api", tags=["search"])


def _chunk_payload(chunk: Any, *, rank: int, max_chars: int, expanded_chunk: Any | None = None) -> dict[str, Any]:
    meta = dict(getattr(chunk, "meta", {}) or {})
    content = str(getattr(chunk, "content", "") or "")
    expanded_content = str(getattr(expanded_chunk, "content", "") or "") if expanded_chunk is not None else ""
    return {
        "rank": rank,
        "score": round(float(getattr(chunk, "score", 0.0) or 0.0), 4),
        "doc_id": getattr(chunk, "doc_id", ""),
        "doc_name": getattr(chunk, "doc_name", ""),
        "content": content[:max_chars],
        "content_truncated": len(content) > max_chars,
        "metadata": meta,
        "doc_type": meta.get("doc_type"),
        "content_type": meta.get("content_type"),
        "source_id": meta.get("source_id") or meta.get("GlobalId") or meta.get("global_id"),
        "retrieval_sources": meta.get("retrieval_sources"),
        "rrf_rank": meta.get("rrf_rank"),
        "rrf_score": meta.get("rrf_score"),
        "context": {
            "content": expanded_content[:max_chars],
            "content_truncated": len(expanded_content) > max_chars,
            "metadata": dict(getattr(expanded_chunk, "meta", {}) or {}) if expanded_chunk is not None else {},
        }
        if expanded_chunk is not None
        else None,
    }


@search_router.post("/search")
async def search(req: SearchRequest, _user=Depends(require_user)):
    state = dataset_runtime.get_dataset_state()
    query = req.effective_query()
    if not query:
        raise HTTPException(status_code=400, detail="query or question is required")

    effective_dataset_filter = req.dataset_filter
    dataset_ids = await resolve_dataset_ids(
        state.backend,
        req.dataset_ids,
        effective_dataset_filter,
        logger,
        question=query,
    )
    retrieval = await retrieve_chat_chunks(
        question=query,
        dataset_ids=dataset_ids,
        rag_backend=state.backend,
        reranker_enabled=req.reranker_enabled,
        reranker_available=True,
        reranker_cls=select_reranker_cls(),
        mlx_url=mlx_url(),
        logger=logger,
        return_trace=True,
    )
    chunks = retrieval.chunks[: req.top_k]
    expanded_chunks: list[Any] = []
    context_payload: dict[str, Any] | None = None
    if req.include_context:
        context_windows = expand_context_windows(
            chunks,
            collection=getattr(state.backend, "collection_name", ""),
            logger=logger,
            max_chunks=req.top_k,
        )
        expanded_chunks = list(context_windows.chunks)
        context_payload = context_windows.payload()

    result: dict[str, Any] = {
        "query": query,
        "dataset_filter": effective_dataset_filter,
        "dataset_ids": dataset_ids,
        "top_k": req.top_k,
        "count": len(chunks),
        "route": {
            "dataset_filter": effective_dataset_filter,
            "reason": "explicit_filter" if req.dataset_filter else "all_corpus",
            "expanded": False,
            "kot": retrieval.kot.payload(),
        },
        "chunks": [
            _chunk_payload(
                chunk,
                rank=index + 1,
                max_chars=req.max_chars,
                expanded_chunk=expanded_chunks[index] if index < len(expanded_chunks) else None,
            )
            for index, chunk in enumerate(chunks)
        ],
    }
    if req.include_trace:
        trace = retrieval.payload()
        if context_payload is not None:
            trace["context_window"] = context_payload
        result["retrieval_trace"] = trace
        result["embedding"] = rag_runtime_config()
    return result


@router.post("/retrieve-debug")
async def retrieve_debug(req: RetrievalDebugRequest, _user=Depends(require_user)):
    state = dataset_runtime.get_dataset_state()
    query_route = classify_query(req.question)
    dataset_ids = await resolve_dataset_ids(
        state.backend,
        req.dataset_ids,
        req.dataset_filter,
        logger,
        question=req.question,
    )
    # W2.3: retrieve-debug идёт ТЕМ ЖЕ путём, что чат (гибрид → реранк) —
    # иначе гейты и граф «куда смотрит RAG» видят не то, что пользователь.
    try:
        from backend.reranker import select_reranker_cls

        _rr_cls = select_reranker_cls()
        _rr_available = True
    except ImportError:
        _rr_cls, _rr_available = None, False
    _rr_enabled = req.reranker_enabled
    retrieval = await retrieve_chat_chunks(
        question=req.question,
        dataset_ids=dataset_ids,
        rag_backend=state.backend,
        reranker_enabled=_rr_enabled,
        reranker_available=_rr_available,
        reranker_cls=_rr_cls,
        mlx_url=mlx_url(),
        logger=logger,
        return_trace=True,
    )
    chunks = retrieval.chunks[: req.top_k]
    context_windows = expand_context_windows(
        chunks,
        collection=getattr(state.backend, "collection_name", ""),
        logger=logger,
        max_chunks=req.top_k,
    )
    retrieval_trace = retrieval.payload()
    retrieval_trace["context_window"] = context_windows.payload()
    expanded_chunks = list(context_windows.chunks)
    return {
        "question": req.question,
        "query_route": {
            "dataset_filter": req.dataset_filter or query_route.dataset_filter,
            "reason": query_route.reason,
            "expanded": query_route.expanded_query != req.question,
            "kot": retrieval.kot.payload(),
        },
        "dataset_ids": dataset_ids,
        "embedding": rag_runtime_config(),
        "retrieval_trace": retrieval_trace,
        "top_k": req.top_k,
        "chunks": [
            {
                "rank": index + 1,
                "score": round(float(getattr(chunk, "score", 0.0) or 0.0), 4),
                # Debug/evaluation output is evidence too: it must be a faithful
                # projection of the retrieved object, never an expected-term shim.
                "doc_name": getattr(chunk, "doc_name", ""),
                "doc_id": getattr(chunk, "doc_id", ""),
                "doc_type": (getattr(chunk, "meta", {}) or {}).get("doc_type"),
                "content_type": (getattr(chunk, "meta", {}) or {}).get("content_type"),
                "rrf_rank": (getattr(chunk, "meta", {}) or {}).get("rrf_rank"),
                "rrf_score": (getattr(chunk, "meta", {}) or {}).get("rrf_score"),
                "retrieval_sources": (getattr(chunk, "meta", {}) or {}).get("retrieval_sources"),
                "context_expanded": (getattr(expanded_chunks[index], "meta", {}) or {}).get(
                    "context_expanded",
                    False,
                )
                if index < len(expanded_chunks)
                else False,
                "preview": getattr(chunk, "content", "")[:1000],
                "expanded_preview": getattr(
                    expanded_chunks[index] if index < len(expanded_chunks) else chunk,
                    "content",
                    getattr(chunk, "content", ""),
                )[:1200],
            }
            for index, chunk in enumerate(chunks)
        ],
    }
