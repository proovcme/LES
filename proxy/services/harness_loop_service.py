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


class HarnessLoop:
    def __init__(self, *, max_calls, deadline_seconds):
        self._state = native_core.required('harness', request=dict(
            action='new', max_calls=max_calls, deadline_seconds=deadline_seconds))['state']

    def _step(self, action, **arguments):
        result = native_core.required('harness', request=dict(
            action=action, state=self._state, **arguments))
        self._state = result['state']
        return result

    @property
    def remaining(self):
        return self._state['remaining']

    @property
    def round(self):
        return self._state['round']

    @property
    def reason(self):
        return self._state['reason']

    @property
    def stopped(self):
        return self._state['stopped']

    def enter_round(self):
        return self._step('round')['allowed']

    def select(self, identities):
        return self._step('select', identities=identities)['indexes']

    def begin(self, identity):
        return self._step('begin', identity=identity)['allowed']

    def finish(self, *, failed):
        self._step('finish', failed=failed)
