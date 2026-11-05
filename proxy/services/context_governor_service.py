"""One token-aware packer for canonical model inference context."""

from __future__ import annotations

import json
import math
from dataclasses import dataclass
from enum import Enum
from typing import Any, Callable, Sequence

from backend import native_core
from proxy.services.model_execution_preset_service import ModelExecutionPreset


class ContextKind(str, Enum):
    PROFILE_PREFIX = "profile_prefix"
    TOOL_SHORTLIST = "tool_shortlist"
    REQUEST = "request"
    EVIDENCE = "evidence"
    SOURCE_MAP = "source_map"
    TOOL_EXCHANGE = "tool_exchange"
    CHECKPOINT = "checkpoint"
    WORKING_MEMORY = "working_memory"
    DIALOGUE = "dialogue"
    NATIVE_TOOL_EXCHANGE = "native_tool_exchange"


@dataclass(frozen=True)
class ContextObject:
    object_id: str
    payload: Any

    def render(self) -> str:
        if isinstance(self.payload, str):
            return self.payload
        return json.dumps(
            self.payload,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )


@dataclass(frozen=True)
class ContextCandidate:
    kind: ContextKind
    objects: tuple[ContextObject, ...]
    required: bool = False


@dataclass(frozen=True)
class ContextSection:
    kind: ContextKind
    objects: tuple[ContextObject, ...]
    token_count: int

    @property
    def object_ids(self) -> tuple[str, ...]:
        return tuple(item.object_id for item in self.objects)

    def render(self) -> str:
        return "\n".join(item.render() for item in self.objects)


@dataclass(frozen=True)
class ContextOmission:
    kind: ContextKind
    total: int
    omitted: int
    object_ids: tuple[str, ...]
    cursor: str
    reason: str


@dataclass(frozen=True)
class ContextPacket:
    preset_id: str
    input_budget_tokens: int
    generation_reserve_tokens: int
    safety_reserve_tokens: int
    included_tokens: int
    sections: tuple[ContextSection, ...]
    omissions: tuple[ContextOmission, ...]

    def as_messages(self, *, request_last: bool = False) -> list[dict[str, Any]]:
        sections = []
        for section in self.sections:
            row = dict(kind=section.kind.value, render=section.render())
            if section.kind == ContextKind.NATIVE_TOOL_EXCHANGE:
                row['native'] = [message for item in section.objects for message in item.payload]
            if section.kind == ContextKind.DIALOGUE:
                row['dialogue'] = [
                    dict(role=item.payload['role'], content=str(item.payload.get('content') or ''))
                    if isinstance(item.payload, dict) and item.payload.get('role') in {'user', 'assistant'}
                    else dict(role='user', content=item.render()) for item in section.objects]
            sections.append(row)
        return native_core.required('context', request=dict(
            action='messages', sections=sections, request_last=request_last))['messages']


class ContextRequiredSectionOverflow(ValueError):
    code = "CONTEXT_REQUIRED_SECTION_OVERFLOW"

    def __init__(self, *, object_ids: tuple[str, ...], budget: int, required_tokens: int):
        self.object_ids = object_ids
        self.budget = budget
        self.required_tokens = required_tokens
        super().__init__(
            f"required context needs {required_tokens} tokens but budget is {budget}"
        )


def conservative_token_estimate(text: str) -> int:
    """Provider-neutral conservative estimate until an exact tokenizer is bound."""
    return max(1, math.ceil(len(str(text or "")) / 2))



class ContextGovernor:
    def __init__(
        self,
        preset: ModelExecutionPreset,
        *,
        estimate_tokens: Callable[[str], int] = conservative_token_estimate,
    ) -> None:
        self.preset = preset
        self.estimate_tokens = estimate_tokens

    def _object_tokens(self, item: ContextObject) -> int:
        return max(0, int(self.estimate_tokens(item.render())))

    def pack(self, candidates: Sequence[ContextCandidate]) -> ContextPacket:
        """Rust selects whole objects; the bridge preserves public Python dataclasses."""
        objects = []
        rows = []
        for candidate in candidates:
            items = []
            for item in candidate.objects:
                items.append(dict(id=item.object_id, render=item.render(),
                                  tokens=self._object_tokens(item), index=len(objects)))
                objects.append(item)
            rows.append(dict(kind=candidate.kind.value, required=candidate.required, objects=items))
        result = native_core.required('context', request=dict(
            action='pack', input_limit=int(self.preset.input_token_limit),
            generation_reserve=int(self.preset.generation_reserve_tokens),
            safety_reserve=int(self.preset.safety_reserve_tokens), candidates=rows))
        if result.get('overflow'):
            raise ContextRequiredSectionOverflow(object_ids=tuple(result['object_ids']),
                budget=result['budget'], required_tokens=result['required_tokens'])
        return ContextPacket(
            preset_id=self.preset.preset_id, input_budget_tokens=result['budget'],
            generation_reserve_tokens=self.preset.generation_reserve_tokens,
            safety_reserve_tokens=self.preset.safety_reserve_tokens, included_tokens=result['used'],
            sections=tuple(ContextSection(kind=ContextKind(row['kind']),
                objects=tuple(objects[index] for index in row['indexes']), token_count=row['tokens'])
                for row in result['sections']),
            omissions=tuple(ContextOmission(kind=ContextKind(row['kind']), total=row['total'],
                omitted=row['omitted'], object_ids=tuple(row['object_ids']),
                cursor=row['cursor'], reason=row['reason']) for row in result['omissions']))
