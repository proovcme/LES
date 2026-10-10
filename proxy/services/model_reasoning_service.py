"""Explicit profile reasoning policy and provider-neutral output separation."""
from dataclasses import replace
from typing import Any, Mapping

from proxy.services.model_execution_preset_service import ModelExecutionPreset


def validate_reasoning_policy(policy: Mapping[str, Any]) -> tuple[bool, int]:
    enabled = policy.get("reasoning_enabled", False)
    budget = policy.get("reasoning_budget_tokens", 2048)
    if not isinstance(enabled, bool):
        raise ValueError("reasoning_enabled должен быть логическим значением")
    if isinstance(budget, bool) or not isinstance(budget, int) or not 512 <= budget <= 8192:
        raise ValueError("Бюджет размышления и ответа должен быть от 512 до 8192 токенов")
    return enabled, budget


def profile_execution_preset(
    preset: ModelExecutionPreset, policy: Mapping[str, Any],
) -> ModelExecutionPreset:
    enabled, budget = validate_reasoning_policy(policy or {})
    if not enabled:
        return replace(preset, reasoning_enabled=False)
    if budget + preset.safety_reserve_tokens + 256 > preset.input_token_limit:
        raise ValueError("Бюджет размышления не помещается в контекст модели")
    return replace(
        preset, reasoning_enabled=True, generation_reserve_tokens=budget,
        source_chain=preset.source_chain + ("profile_reasoning_policy",),
    )


class ReasoningOutput:
    """Handle separate fields and inline tags, including a prefilled open tag.

    Unknown inline output is held until its protocol is identified. Reasoning
    text is never retained once an opening delimiter is observed.
    """
    def __init__(self, enabled: bool = False):
        self.mode = "pending" if enabled else "answer"
        self.buffer = ""
        self.observed = False
        self.answer_seen = False

    def separate(self) -> None:
        self.observed = True
        if self.mode == "pending" and not self.buffer:
            self.mode = "answer"

    def feed(self, text: str) -> str:
        if self.mode == "answer":
            self.answer_seen = self.answer_seen or bool(text.strip())
            return text
        self.buffer += text
        if "</think>" in self.buffer:
            _, answer = self.buffer.split("</think>", 1)
            self.buffer = ""
            self.mode = "answer"
            self.observed = True
            self.answer_seen = bool(answer.strip())
            return answer
        if "<think>" in self.buffer:
            self.mode = "thinking"
            self.observed = True
        if self.mode == "thinking":
            self.buffer = self.buffer[-7:]
        return ""

    def finish(self, finish_reason: str) -> str:
        exhausted = finish_reason in {"length", "max_tokens"}
        answer_missing = self.mode == "pending" or (self.observed and not self.answer_seen)
        if self.mode == "thinking" or (exhausted and answer_missing):
            raise ValueError("REASONING_BUDGET_EXHAUSTED")
        text, self.buffer = self.buffer, ""
        self.mode = "answer"
        return text
