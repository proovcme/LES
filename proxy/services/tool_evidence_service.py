"""Request-local adapter to native budget/deadline/replay control."""
import hashlib

from backend import native_core


def _evidence_id(chunk):
    meta = getattr(chunk, "meta", {}) or {}
    identity = str(meta.get("chunk_id") or "")
    if identity:
        return identity
    content = str(getattr(chunk, "content", "") or "")
    name = str(getattr(chunk, "doc_name", "") or "")
    return hashlib.sha256(f"{name}\0{content}".encode("utf-8")).hexdigest()


def admitted_tool_evidence(existing, proposed, exact_documents):
    descriptors = [
        {"id": _evidence_id(chunk), "dataset": str((getattr(chunk, "meta", {}) or {}).get("dataset_id") or ""),
         "file": str(getattr(chunk, "doc_name", "") or "")}
        for chunk in proposed
    ]
    selected = native_core.required("harness_evidence", request={
        "action": "merge_tool_evidence", "existing": [_evidence_id(chunk) for chunk in existing],
        "proposed": descriptors, "exact": sorted(exact_documents),
    })["indexes"]
    return [proposed[index] for index in selected]
