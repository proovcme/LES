"""Typed compatibility API; capacity and narrowing rules are owned by Rust."""
from __future__ import annotations
from dataclasses import dataclass, asdict
import json
from pathlib import Path
from typing import Any, Mapping
from backend import native_core

@dataclass(frozen=True)
class BackendCapacity:
    provider: str
    model_id: str
    context_tokens: int | None
    observed: bool
    source: str

@dataclass(frozen=True)
class ModelExecutionPreset:
    preset_id: str
    model_family: str
    input_token_limit: int
    generation_reserve_tokens: int
    safety_reserve_tokens: int
    normal_tool_count: int
    max_tools: int
    max_batch_items: int
    parallel_read_limit: int
    reasoning_enabled: bool
    source_chain: tuple[str, ...]

    def diagnostics(self, *, requested_input_tokens: int | None = None) -> dict[str, Any]:
        """Return operator-safe requested/effective/source values."""
        source = " > ".join(self.source_chain)
        return {
            "model_preset": {
                "requested": self.model_family,
                "effective": self.preset_id,
                "source": source,
                "restart_required": False,
            },
            "context_input_tokens": {
                "requested": requested_input_tokens,
                "effective": self.input_token_limit,
                "source": source,
                "restart_required": False,
            },
            "generation_reserve": {
                "requested": None,
                "effective": self.generation_reserve_tokens,
                "source": source,
                "restart_required": False,
            },
            "safety_reserve": {
                "requested": None,
                "effective": self.safety_reserve_tokens,
                "source": "workflow_invariants",
                "restart_required": False,
            },
            "reasoning": {
                "requested": self.reasoning_enabled,
                "effective": self.reasoning_enabled,
                "source": source,
                "restart_required": False,
            },
        }

def _preset(value):
    return ModelExecutionPreset(**{**value, 'source_chain': tuple(value['source_chain'])})

_FACTORIES = json.loads((Path(__file__).resolve().parents[2] / 'config/execution-presets.json').read_text(encoding='utf-8'))
_FACTORY_9B = _preset(_FACTORIES['restrictive'])
_FACTORY_35B = _preset(_FACTORIES['extended'])


def resolve_execution_preset(capacity: BackendCapacity, *, operator: Mapping[str, Any] | None = None,
                             restrictions: Mapping[str, Any] | None = None) -> ModelExecutionPreset:
    value = native_core.required('model_profile', request={
        'schema': 'les.model-profile.v1', 'capacity': asdict(capacity),
        'operator': dict(operator) if operator is not None else None,
        'restrictions': dict(restrictions) if restrictions is not None else None,
    })
    return _preset(value['preset'])
