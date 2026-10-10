"""Source excerpts, tool arguments and request context helpers."""
from __future__ import annotations
import logging
import re
import sqlite3
import json
from typing import Any, Iterable
from backend.rag_config import rag_meta_db_path
from proxy.services.llm_transport_profile_service import provider_prompt_max_chars, provider_is_local
from proxy.services import chat_inference_service

logger = logging.getLogger(__name__)


def clean_visible_text(text: str) -> str:
    """Normalize presentation whitespace without deleting valid source scripts."""
    cleaned = str(text or "")
    cleaned = re.sub(r"[ \t]{2,}", " ", cleaned)
    cleaned = re.sub(r"\n{3,}", "\n\n", cleaned)
    return cleaned.strip()


def source_excerpts(chunks, *, max_n: int = 6, max_chars: int = 700) -> list[dict[str, Any]]:
    """Конкретные фрагменты источников (текст, а не только имя файла) — чтобы
    показать «вот это место в норме» под ответом. Дедуп по (документ, начало)."""
    out: list[dict[str, Any]] = []
    seen: set = set()
    for ch in chunks or []:
        content = clean_visible_text((getattr(ch, "content", "") or "").strip())
        if not content:
            continue
        doc = getattr(ch, "doc_name", "") or ""
        key = (doc, content[:80])
        if key in seen:
            continue
        seen.add(key)
        if len(content) > max_chars:
            content = content[:max_chars].rsplit(" ", 1)[0].rstrip() + " …"
        meta = getattr(ch, "meta", {}) or {}
        out.append({
            "doc": doc,
            "text": content,
            "score": round(float(getattr(ch, "score", 0.0) or 0.0), 3),
            "dataset_id": meta.get("dataset_id", "") if isinstance(meta, dict) else "",
        })
        if len(out) >= max_n:
            break
    return out


def _extract_json_object(text: str) -> dict[str, Any] | None:
    raw = str(text or "").strip()
    if not raw:
        return None
    fence = re.search(r"```(?:json)?\s*(\{.*?\})\s*```", raw, flags=re.DOTALL | re.IGNORECASE)
    if fence:
        raw = fence.group(1).strip()
    start = raw.find("{")
    end = raw.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        parsed = json.loads(raw[start:end + 1])
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _parse_model_tool_calls(text: str, *, allowed_tools: set[str], max_calls: int = 3) -> list[dict[str, Any]]:
    parsed = _extract_json_object(text)
    if not parsed:
        return []
    calls_raw = parsed.get("calls")
    if isinstance(calls_raw, dict):
        calls_raw = [calls_raw]
    if not isinstance(calls_raw, list):
        return []
    calls: list[dict[str, Any]] = []
    for item in calls_raw:
        if not isinstance(item, dict):
            continue
        tool = str(item.get("tool") or item.get("name") or "").strip()
        if tool not in allowed_tools:
            continue
        args = item.get("args") if isinstance(item.get("args"), dict) else {}
        parsed_call = {"tool": tool, "args": dict(args)}
        call_id = str(item.get("call_id") or item.get("id") or "")
        if call_id:
            parsed_call["call_id"] = call_id
        calls.append(parsed_call)
        if len(calls) >= max(1, max_calls):
            break
    return calls


def _augment_model_tool_args(
    call: dict[str, Any],
    *,
    question: str,
    dataset_ids: list[str],
    target_file_ref: dict[str, Any] | None,
) -> dict[str, Any]:
    tool = str(call.get("tool") or "")
    args = dict(call.get("args") or {})
    if tool == "dataset_map" and dataset_ids and not args.get("dataset_id"):
        args["dataset_id"] = dataset_ids[0]
    if tool in {"search_sources", "read_source", "read_pdf_source", "read_excel_source", "look_at_pdf_page"}:
        if (
            tool != "search_sources"
            and question
            and not (args.get("q") or args.get("question"))
        ):
            args["question" if tool == "look_at_pdf_page" else "q"] = question
        if dataset_ids:
            if tool == "search_sources" and not args.get("dataset_ids") and not args.get("dataset_id"):
                args["dataset_ids"] = dataset_ids
            elif tool != "search_sources" and not args.get("dataset_id") and not args.get("doc_id"):
                args["dataset_id"] = dataset_ids[0]
        if target_file_ref and target_file_ref.get("match_status") == "matched":
            if not args.get("doc_id") and not args.get("doc_name"):
                args["doc_name"] = target_file_ref.get("file_name") or ""
            if not args.get("doc_id") and target_file_ref.get("dataset_id"):
                args["dataset_id"] = target_file_ref.get("dataset_id")
    augmented = {"tool": tool, "args": args}
    if call.get("call_id"):
        augmented["call_id"] = str(call["call_id"])
    return augmented


def _compact_tool_result_for_prompt(payload: dict[str, Any], *, max_chars: int = 7000) -> dict[str, Any]:
    # Skill pages and extension schemas must remain whole: cutting their JSON or
    # text would silently skip instructions while retaining a later next_offset.
    # The context governor admits or rejects whole objects at the outer boundary.
    tool = str(payload.get('tool') or '')
    structured_extension = tool.startswith('use_extensions_') or tool in {
        'list_installed_skills', 'read_installed_skill', 'run_skill_calculation', 'read_skill_calculation'}
    if structured_extension:
        result = payload.get('result') or {}
        if isinstance(result, dict) and result.get('schema') == 'les_tool_result_v1':
            result = {key: result[key] for key in ('tool', 'status', 'result', 'sources', 'missing', 'warnings') if key in result}
        return {'tool': tool, 'status': payload.get('status'), 'result': result}
    keep = {
        "tool": payload.get("tool"),
        "status": payload.get("status"),
        "result": payload.get("result") or {},
        "sources": payload.get("sources") or [],
        "missing": payload.get("missing") or [],
        "warnings": payload.get("warnings") or [],
        "trace": payload.get("trace") or "",
    }
    text = json.dumps(keep, ensure_ascii=False, default=str)
    if len(text) <= max_chars:
        return keep
    trimmed = dict(keep)
    trimmed["result"] = {
        "summary": "tool result trimmed for prompt only; full result is in retrieval_trace.tool_loop",
        "text": text[:max_chars].rsplit(" ", 1)[0].rstrip(),
        "prompt_truncated": True,
    }
    return trimmed


def _format_tool_results_for_model(results: list[dict[str, Any]]) -> str:
    if not results:
        return ""
    compacted = [
        _compact_tool_result_for_prompt(
            result,
            max_chars=max(1000, chat_inference_service._env_int("LES_CHAT_TOOL_RESULT_PROMPT_CHARS", 7000)),
        )
        for result in results
    ]
    return (
        "РЕЗУЛЬТАТЫ ИНСТРУМЕНТОВ LES (read-only; это материалы для модели, не готовый ответ):\n"
        + json.dumps(compacted, ensure_ascii=False, indent=2, default=str)
    )


def _local_context_budget(
    *,
    local_big: bool,
    big_context: bool,
    provider: str = "",
) -> dict[str, int]:
    """Context budget for chat generation.

    Cloud can digest a large prompt quickly. Local MLX pays heavily for prefill,
    so technical/legal RAG gets a smaller default budget with env overrides.
    """
    if provider_is_local(provider):
        prompt_chars = provider_prompt_max_chars(provider)
        return {
            "focus_max_chunks": chat_inference_service._env_int("FREETOKEN_FOCUS_MAX_CHUNKS", 0),
            "context_max_chunks": chat_inference_service._env_int("FREETOKEN_CONTEXT_MAX_CHUNKS", 0),
            "context_chars_limit": chat_inference_service._env_int("FREETOKEN_EVIDENCE_MAX_CHARS", prompt_chars),
            "context_window_chars": chat_inference_service._env_int("FREETOKEN_CONTEXT_WINDOW_CHARS", 1800),
        }
    return {
        "focus_max_chunks": 0,
        "context_max_chunks": 0,
        "context_chars_limit": chat_inference_service._env_int("RAG_MODEL_CONTEXT_CHARS", 120000),
        "context_window_chars": chat_inference_service._env_int("RAG_CONTEXT_WINDOW_CHARS", 4000),
    }


def _generation_token_budget(*, max_tokens: int, local_big: bool, attempt: int, intent: str) -> int:
    if attempt != 1:
        return chat_inference_service._env_int("RAG_CHAT_RETRY_MAX_TOKENS", 2048)
    if not local_big:
        return max_tokens
    if intent in {"default", "full"}:
        return max_tokens
    cap = chat_inference_service._env_int("RAG_LOCAL_CHAT_MAX_TOKENS", 1100)
    return min(max_tokens, cap)


def _dataset_sensitivities(dataset_ids: Iterable[str]) -> list[str]:
    """Уровни чувствительности (P0/P1/P2) задействованных датасетов из метабазы.

    Fail-closed: БД/колонка недоступны или хоть один датасет не найден → P0
    (приватно), чтобы политика W3.3 никогда не открыла облако по ошибке чтения.
    """
    ids = [str(d).strip() for d in dataset_ids if str(d).strip()]
    if not ids:
        return []
    try:
        with sqlite3.connect(rag_meta_db_path()) as conn:
            placeholders = ",".join("?" for _ in ids)
            rows = conn.execute(
                f"SELECT sensitivity FROM datasets WHERE id IN ({placeholders})",
                ids,
            ).fetchall()
        levels = [r[0] for r in rows]
        if len(levels) < len(ids):  # неизвестный датасет → считаем приватным
            levels.append("P0")
        return levels or ["P0"]
    except Exception as exc:  # noqa: BLE001 — любая ошибка чтения → приватно
        logger.warning("[ROUTE] sensitivity read failed (%s) — fail-closed P0", exc)
        return ["P0"]


def _dataset_ids_from_chunks(chunks: list[Any]) -> list[str]:
    ids: list[str] = []
    seen: set[str] = set()
    for chunk in chunks:
        meta = getattr(chunk, "meta", {}) or {}
        dataset_id = str(meta.get("dataset_id") or "").strip()
        if dataset_id and dataset_id not in seen:
            ids.append(dataset_id)
            seen.add(dataset_id)
    return ids


async def _dataset_name_map(rag_backend) -> dict[str, str]:
    try:
        datasets = await rag_backend.list_datasets()
    except Exception:
        return {}
    return {str(dataset.id): str(dataset.name) for dataset in datasets}


def _names_for_dataset_ids(dataset_ids: list[str] | None, name_by_id: dict[str, str]) -> list[str]:
    return [name_by_id.get(str(dataset_id), str(dataset_id)) for dataset_id in (dataset_ids or [])]


def _query_route_payload(query_intent: Any, effective_dataset_filter: str | None, kot_decision: Any) -> dict[str, Any]:
    return {
        "channel": query_intent.channel,
        "reason": query_intent.reason,
        "dataset_filter": effective_dataset_filter,
        "kot": kot_decision.payload(),
    }
