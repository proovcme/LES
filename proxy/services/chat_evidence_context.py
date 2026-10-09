"""Evidence context budgets, source boundaries and model prompt assembly."""
from __future__ import annotations
import hashlib
import json
import logging
import re
from typing import Any, Sequence
from proxy.services.context_governor_service import ContextCandidate, ContextGovernor, ContextKind, ContextObject, ContextPacket
from proxy.services.prompt_registry_service import build_mode_system_prompt
from proxy.services.model_execution_preset_service import ModelExecutionPreset




def _context_objects(
    prefix: str,
    values: Sequence[Any],
) -> tuple[ContextObject, ...]:
    """Create stable, whole context objects; never slice an object to make it fit."""
    objects: list[ContextObject] = []
    for index, value in enumerate(values):
        if value in (None, "", [], {}, ()):
            continue
        objects.append(ContextObject(f"{prefix}:{index}", value))
    return tuple(objects)


def _text_context_objects(prefix: str, text: str) -> tuple[ContextObject, ...]:
    """Split only at producer-owned paragraph boundaries, preserving every paragraph."""
    return _context_objects(
        prefix,
        [part.strip() for part in str(text or "").split("\n\n") if part.strip()],
    )


def workspace_memory_objects(
    candidates: Sequence[ContextCandidate], *, registered: bool,
) -> tuple[ContextObject, ...]:
    """User notes and inspectable conversation summaries are advisory, never evidence."""
    if not registered:
        return ()
    return tuple(
        item for candidate in candidates
        for item in candidate.objects
        if (candidate.kind == ContextKind.WORKING_MEMORY and item.object_id.startswith("note:"))
        or (candidate.kind == ContextKind.CHECKPOINT and item.object_id.startswith("conversation-summary:"))
    )


def govern_inference_messages(
    *,
    preset: ModelExecutionPreset,
    profile_prefix: str,
    request_payload: Any,
    shortlist: Sequence[Any] = (),
    checkpoint: Sequence[ContextObject] = (),
    working_memory: Sequence[ContextObject] = (),
    evidence: Sequence[Any] = (),
    source_map: Sequence[Any] = (),
    tool_exchange: Sequence[Any] = (),
    dialogue: Sequence[Any] = (),
    required_evidence: Sequence[Any] = (),
    native_tool_exchange: Sequence[Any] = (),
) -> tuple[list[dict[str, str]], ContextPacket]:
    """Build the sole bounded packet used for one provider inference request."""
    candidates = [
        ContextCandidate(
            ContextKind.PROFILE_PREFIX,
            (ContextObject("profile:bound", profile_prefix),),
            required=True,
        ),
        ContextCandidate(ContextKind.TOOL_SHORTLIST, _context_objects("tool", shortlist)),
        ContextCandidate(
            ContextKind.REQUEST,
            (ContextObject("request:current", request_payload),),
            required=True,
        ),
        ContextCandidate(ContextKind.CHECKPOINT, tuple(checkpoint)),
        ContextCandidate(ContextKind.WORKING_MEMORY, tuple(working_memory)),
        ContextCandidate(ContextKind.EVIDENCE, _context_objects("evidence", evidence)),
        ContextCandidate(ContextKind.EVIDENCE, _context_objects("current-document", required_evidence), required=True),
        ContextCandidate(ContextKind.NATIVE_TOOL_EXCHANGE, _context_objects("current-tool-turn", native_tool_exchange), required=True),
        ContextCandidate(ContextKind.SOURCE_MAP, _context_objects("source", [
            {key: item[key] for key in ("index", "label", "doc_name", "source_page", "page", "url",
                                      "dataset_id", "qdrant_point_id", "parent_id", "section_fragment_count",
                                      "context_origin") if key in item}
            if isinstance(item, dict) else item for item in source_map
        ])),
        ContextCandidate(ContextKind.TOOL_EXCHANGE, _context_objects("exchange", tool_exchange)),
        ContextCandidate(ContextKind.DIALOGUE, _context_objects("dialogue", dialogue), required=bool(dialogue)),
    ]
    packet = ContextGovernor(preset).pack(candidates)
    # A label without its evidence must not be advertised to the model either.
    # EVIDENCE precedes optional SOURCE_MAP, so this second pass cannot evict
    # or reintroduce evidence. It only removes dangling source references.
    visible = {item.get("index") for item in model_visible_source_map(packet, source_map)
               if isinstance(item, dict)}
    candidates = [
        ContextCandidate(candidate.kind, tuple(
            obj for obj in candidate.objects
            if not isinstance(obj.payload, dict) or not obj.payload.get("index")
            or obj.payload["index"] in visible), required=candidate.required)
        if candidate.kind == ContextKind.SOURCE_MAP else candidate
        for candidate in candidates
    ]
    packet = ContextGovernor(preset).pack(candidates)
    return packet.as_messages(request_last=True), packet


def context_packet_trace(packet: ContextPacket, *, purpose: str) -> dict[str, Any]:
    """Expose exact model-visible evidence while keeping private prompt/memory redacted."""
    visible_kinds = {ContextKind.EVIDENCE, ContextKind.SOURCE_MAP, ContextKind.NATIVE_TOOL_EXCHANGE}

    def section_trace(section) -> dict[str, Any]:
        item = {
            "kind": section.kind.value,
            "items": len(section.objects),
            "tokens": section.token_count,
            "object_ids": list(section.object_ids),
        }
        if section.kind in visible_kinds:
            item["objects"] = [
                {
                    "object_id": obj.object_id,
                    "payload": json.loads(json.dumps(obj.payload, ensure_ascii=False, default=str)),
                    "text": obj.render(),
                    "sha256": hashlib.sha256(obj.render().encode("utf-8")).hexdigest(),
                }
                for obj in section.objects
            ]
        return item

    return {
        "purpose": purpose,
        "preset_id": packet.preset_id,
        "input_budget_tokens": packet.input_budget_tokens,
        "generation_reserve_tokens": packet.generation_reserve_tokens,
        "safety_reserve_tokens": packet.safety_reserve_tokens,
        "included_tokens": packet.included_tokens,
        "sections": [section_trace(section) for section in packet.sections],
        "omissions": [
            {
                "kind": omission.kind.value,
                "total": omission.total,
                "omitted": omission.omitted,
                "object_ids": list(omission.object_ids),
                "cursor": omission.cursor,
                "reason": omission.reason,
            }
            for omission in packet.omissions
        ],
    }


def profile_temperature(profile_snapshot: dict[str, Any] | None, *, fallback: float) -> float:
    """Return the bounded immutable profile temperature for generation."""

    policy = (profile_snapshot or {}).get("model_policy") or {}
    try:
        value = float(policy.get("temperature", fallback))
    except (TypeError, ValueError):
        value = fallback
    return max(0.0, min(2.0, value))


def profile_research_rounds(profile_snapshot: dict[str, Any] | None, *, configured: int) -> int:
    """Respect the profile's iterative-search switch without changing the global ceiling."""

    iterative = bool(((profile_snapshot or {}).get("rag_policy") or {}).get("iterative", True))
    return max(1, configured) if iterative else 1


def _bounded_source_blocks(text: str, *, max_chars: int = 700) -> list[str]:
    """Keep source rows intact while making a large attachment packable."""

    blocks: list[str] = []
    current: list[str] = []
    current_chars = 0
    for raw_line in str(text or "").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        added = len(line) + (1 if current else 0)
        if current and current_chars + added > max_chars:
            blocks.append("\n".join(current))
            current = []
            current_chars = 0
        if len(line) > max_chars:
            if current:
                blocks.append("\n".join(current))
                current = []
                current_chars = 0
            blocks.append(line)
            continue
        current.append(line)
        current_chars += len(line) + (1 if current_chars else 0)
    if current:
        blocks.append("\n".join(current))
    return blocks


def selector_evidence_payload(
    *, attachment_context: str, rendered_context: str,
) -> list[Any]:
    """Return model evidence as independently packable, ordered source blocks."""

    payload: list[Any] = []
    if str(attachment_context or "").strip():
        payload.append("Текст явно прикреплённого пользователем файла:")
        payload.extend(_bounded_source_blocks(attachment_context))
    if str(rendered_context or "").strip():
        payload.append("Материалы из найденных документов:")
    payload.extend(source_context_blocks(rendered_context))
    return payload


def source_context_blocks(text: str) -> list[str]:
    """A source header and its paragraphs enter or leave the model together."""
    return [part.strip() for part in re.split(r"(?m)(?=^\[Источник \d+(?:\s|\]))", text) if part.strip()]


def model_visible_source_map(packet: ContextPacket, source_map: Sequence[Any]) -> list[Any]:
    """Never report a dropped source as evidence that the answerer saw."""
    indexes = set()
    for section in packet.sections:
        if section.kind == ContextKind.EVIDENCE:
            for obj in section.objects:
                indexes.update(int(n) for n in re.findall(r"(?m)^\[Источник (\d+)(?:\s|\])", obj.render()))
    return [source for source in source_map
            if not isinstance(source, dict) or not source.get("index") or source.get("index") in indexes]


def selector_context_shortlist(
    shortlist: Sequence[Any], *, native_tool_schemas: bool,
) -> Sequence[Any]:
    """Do not duplicate native provider tool schemas inside message context."""

    return () if native_tool_schemas else shortlist


def initial_selector_context(
    rendered_context: str, *, model_authored_initial_query: bool,
) -> str:
    """Do not label a not-yet-run model-authored search as empty retrieval."""

    return "" if model_authored_initial_query else rendered_context


def profile_system_prompt(profile_snapshot: dict[str, Any] | None, *, strict: bool) -> str:
    """Compile the exact per-chat prompt/skill snapshot for grounded generation."""

    snapshot = profile_snapshot if isinstance(profile_snapshot, dict) else {}
    prompt = str(snapshot.get("prompt_text") or "").strip()
    skill = str(snapshot.get("skill_text") or "").strip()
    if not prompt:
        prompt = build_mode_system_prompt("rag")
    parts = [prompt]
    if skill:
        parts.append("Инструкции выбранного профиля:\n" + skill)
    for item in snapshot.get("additional_skills") or []:
        parts.append(str(item["text"]))
    parts.append("Ссылки на материалы с метками [Источник N] оформляй точно этими метками; "
                 "для веб-страниц используй предоставленный URL.")
    return "\n\n".join(parts)


def profile_tool_selector_prompt(profile_snapshot: dict[str, Any] | None) -> str:
    """Compile the thin role+skill contract for one native tool decision."""

    snapshot = profile_snapshot if isinstance(profile_snapshot, dict) else {}
    mode = str(snapshot.get("mode") or "agent").strip() or "agent"
    skill = str(snapshot.get("skill_text") or "").strip()
    parts = [
        (
            f"Ты — Л.Е.С., профиль {mode}. На этом вызове выбери нужные native tools, "
            "а не формулируй итоговый ответ. Модель сама создаёт поисковые запросы и "
            "принимает предметные решения; код только исполняет вызовы и оформляет результат."
        )
    ]
    if skill:
        parts.append("Активный skill профиля:\n" + skill)
    return "\n\n".join(parts)
