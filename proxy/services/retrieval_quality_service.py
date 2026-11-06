"""Cheap retrieval quality checks before generation."""

from __future__ import annotations

from dataclasses import dataclass
import math
from typing import Any

from backend import native_core
from backend.inference.lexical_tokens import _STOP_WORDS, normalized_text
from proxy.services.kot_service import KotDecision
from proxy.services.lexical_index_service import RetrievalTrace

_NATIVE_STOP_WORDS = sorted(_STOP_WORDS)


@dataclass(frozen=True)
class RetrievalQuality:
    status: str
    detail: str
    term_coverage: float
    source_diversity: int
    top_score: float
    score_kind: str = "unknown"
    retry_wider: bool = False

    def payload(self) -> dict[str, Any]:
        return {
            "status": self.status,
            "detail": self.detail,
            "term_coverage": round(self.term_coverage, 3),
            "source_diversity": self.source_diversity,
            "top_score": round(self.top_score, 4),
            "score_kind": self.score_kind,
            "assessment_scope": "retrieval_signals_only",
            "answerability": "no_evidence_found" if self.detail == "no_chunks" else "unverified",
        }


def evaluate_retrieval_quality(
    *,
    question: str,
    chunks: list[Any],
    trace: RetrievalTrace,
    kot: KotDecision,
) -> RetrievalQuality:
    if not chunks:
        return RetrievalQuality("weak", "no_chunks", 0.0, 0, 0.0, trace.score_kind, retry_wider=True)

    names = {str(getattr(chunk, "doc_name", "") or "") for chunk in chunks}
    top_score = float(getattr(chunks[0], "score", 0.0) or 0.0)
    top_score = top_score if math.isfinite(top_score) else 0.0
    result = native_core.required("retrieval_quality", request={
        "question": normalized_text(question),
        "content": normalized_text("\n".join(str(getattr(chunk, "content", "") or "") for chunk in chunks)),
        "names": normalized_text("\n".join(sorted(names))),
        "stop_words": _NATIVE_STOP_WORDS,
        "source_diversity": len(names),
        "top_score": top_score,
        "dense_similarity": trace.score_kind == "dense_similarity",
        "hybrid": "hybrid" in trace.mode,
        "exact_refs": bool(trace.exact_refs),
        "embedding_mismatch": trace.fallback_reason == "embedding_contract_mismatch",
        "ambiguous": bool(kot.ambiguous),
    })
    return RetrievalQuality(result["status"], result["detail"], result["term_coverage"],
                            len(names), top_score, trace.score_kind, result["retry_wider"])


def expanded_quality_query(question: str, kot: KotDecision) -> str:
    """Return the normalized user query without domain-hint injection.

    ``matched_terms`` and ``norm_refs`` remain useful trace/filter hints, but
    appending them to the dense query changes its semantic meaning and can pull
    retrieval away from the user's wording. Weak-query retries may widen the
    candidate pool; they must keep this text contract unchanged.
    """
    del kot
    return " ".join(str(question or "").split())
