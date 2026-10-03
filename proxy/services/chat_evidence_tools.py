"""Model tool schemas, source locators and trace projection."""
from __future__ import annotations
import hashlib
import json
import logging
from dataclasses import dataclass
from typing import Any, Mapping, Sequence
from proxy.services.canonical_route_service import one_model_decision_from_calls
from proxy.services.source_locator_service import source_map_item
from proxy.services.web_research_config_service import WebResearchConfig

logger = logging.getLogger(__name__)

_DOCUMENT_EVIDENCE_TOOLS = frozenset({
    "dataset_map",
    "search_sources",
    "read_source",
    "read_pdf_source",
    "look_at_pdf_page",
    "read_excel_source",
    "search_project_tables",
    "read_project_table",
    "assemble_project_volume",
})


def tool_selector_request_payload(
    *,
    question: str,
    mode: str,
    dataset_ids: Sequence[str],
    target_file_ref: dict[str, Any] | None,
    round_no: int,
    attachment_id: str | None,
) -> dict[str, Any]:
    """Describe the exact operator-bound inputs required for model tool choice."""
    payload: dict[str, Any] = {
        "question": question,
        "mode": mode,
        "dataset_ids": list(dataset_ids),
        "target_file": target_file_ref if target_file_ref else {},
        "round": round_no,
    }
    bound_attachment = str(attachment_id or "").strip()
    if bound_attachment:
        payload["attachment"] = {
            "bound": True,
            "attachment_id": bound_attachment,
        }
    return payload


@dataclass(frozen=True)
class _WebToolSourceChunk:
    content: str
    doc_name: str
    score: float
    meta: dict[str, Any]


def web_tools_for_request(
    tools: Sequence[str], config: WebResearchConfig
) -> list[str]:
    """Expose page reading only for the explicitly captured extended mode."""

    normalized = [str(name) for name in tools if str(name).strip()]
    return normalized


def web_source_map_from_tool_results(
    tool_results: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    """Project actual web evidence into locators without changing its content."""

    chunks_by_url: dict[str, _WebToolSourceChunk] = {}
    for payload in tool_results:
        tool = str(payload.get("tool") or "")
        if tool not in {"web_search", "web_read"}:
            continue
        result = payload.get("result")
        result_data = dict(result) if isinstance(result, Mapping) else {}
        search_rows = result_data.get("results")
        rows = search_rows if isinstance(search_rows, list) else []
        rows_by_url = {
            str(row.get("url") or ""): row
            for row in rows
            if isinstance(row, Mapping) and str(row.get("url") or "")
        }
        for raw_source in payload.get("sources") or []:
            if not isinstance(raw_source, Mapping):
                continue
            url = str(raw_source.get("url") or result_data.get("final_url") or "").strip()
            if not url:
                continue
            row = rows_by_url.get(url, {})
            title = str(
                raw_source.get("title")
                or result_data.get("title")
                or (row.get("title") if isinstance(row, Mapping) else "")
                or ""
            )
            content = str(
                result_data.get("text")
                or (row.get("snippet") if isinstance(row, Mapping) else "")
                or ""
            )
            chunks_by_url[url] = _WebToolSourceChunk(
                content=content,
                doc_name=title,
                score=0.0,
                meta={
                    "url": url,
                    "title": title,
                    "provider": str(result_data.get("provider") or ""),
                    "retrieved_at": str(result_data.get("retrieved_at") or ""),
                },
            )
    return [
        source_map_item(chunk, index=index)
        for index, chunk in enumerate(chunks_by_url.values(), 1)
    ]


def _merge_web_source_map(
    source_map: Sequence[dict[str, Any]],
    tool_results: Sequence[dict[str, Any]],
) -> list[dict[str, Any]]:
    web_sources = web_source_map_from_tool_results(tool_results)
    if not web_sources:
        return list(source_map)
    combined_sources = [
        dict(item)
        for item in source_map
        if str(((item.get("locator") or {}).get("kind") or "")) != "web_result"
    ]
    combined_sources.extend(web_sources)
    for source_index, item in enumerate(combined_sources, 1):
        item["index"] = source_index
        item["label"] = f"Источник {source_index}"
    return combined_sources


def tools_for_document_scope(tools: Sequence[str], *, enabled: bool) -> list[str]:
    """Remove indexed-document tools when the user selected no document scope."""
    normalized = [str(name) for name in tools if str(name).strip()]
    if enabled:
        return normalized
    return [name for name in normalized if name not in _DOCUMENT_EVIDENCE_TOOLS]


def native_model_tool_schemas(
    tool_contracts: Sequence[dict[str, Any]],
) -> list[dict[str, Any]]:
    """Convert registry contracts to provider-native function definitions."""

    schemas: list[dict[str, Any]] = []
    for contract in tool_contracts:
        name = str(contract.get("name") or "").strip()
        parameters = contract.get("input_schema")
        if not name or not isinstance(parameters, dict):
            continue
        schemas.append(
            {
                "type": "function",
                "function": {
                    "name": name,
                    "description": str(contract.get("summary") or name),
                    "parameters": parameters,
                },
            }
        )
    return schemas


def tool_call_identity(call: dict[str, Any]) -> str:
    """Exact request identity for stopping repeated reads in one turn."""
    return json.dumps({"tool": call.get("tool"), "args": call.get("args") or {}},
                      sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def safe_selected_call_trace(call: dict[str, Any]) -> dict[str, str]:
    """Expose a call's shape without retaining model-supplied argument text."""
    arguments = call.get("args") if isinstance(call.get("args"), dict) else {}
    encoded_arguments = json.dumps(
        arguments,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        default=str,
    ).encode("utf-8")
    trace = {
        "tool": str(call.get("tool") or ""),
        "arguments_sha256": hashlib.sha256(encoded_arguments).hexdigest(),
    }
    if call.get("call_id"):
        trace["call_id"] = str(call["call_id"])
    return trace


async def execute_canonical_shadow_decision(
    *,
    proposed_calls: list[dict[str, Any]],
    allowed_tools: set[str],
    dataset_ids: list[str],
    tool_harness: Any,
) -> dict[str, Any]:
    """Execute at most one candidate call and return structural, redacted trace."""
    decision = one_model_decision_from_calls(proposed_calls, allowed=allowed_tools)
    trace: dict[str, Any] = {
        "schema": "les_canonical_shadow_v1",
        "user_visible": False,
        "persisted": False,
        "proposed_calls": decision.proposed_calls,
        "executed_calls": decision.executed_calls,
        "pending_calls": decision.pending_calls,
        "tool_name": str((decision.call or {}).get("tool") or ""),
    }
    if decision.call is None:
        trace.update(status="no_valid_call", execution_code="")
        return trace
    payload = await tool_harness.call_async(
        str(decision.call["tool"]),
        dict(decision.call["args"]),
        actor_id="canonical-shadow",
        actor_role="user",
        allowed_dataset_ids=tuple(str(item) for item in dataset_ids if str(item)),
        shadow=True,
    )
    execution = payload.get("execution") if isinstance(payload, dict) else {}
    trace.update(
        status=str((execution or {}).get("status") or payload.get("status") or "unknown"),
        execution_code=str((execution or {}).get("code") or ""),
        result_schema=str(payload.get("schema") or ""),
    )
    return trace


async def safe_execute_canonical_shadow_decision(**kwargs: Any) -> dict[str, Any]:
    """Keep every candidate failure outside the authoritative legacy path."""
    proposed = kwargs.get("proposed_calls") or []
    allowed = kwargs.get("allowed_tools") or set()
    structural = one_model_decision_from_calls(proposed, allowed=set(allowed))
    try:
        return await execute_canonical_shadow_decision(**kwargs)
    except Exception as error:  # noqa: BLE001 - shadow must never affect legacy
        logger.warning("[CANONICAL_SHADOW] candidate skipped: %s", type(error).__name__)
        return {
            "schema": "les_canonical_shadow_v1",
            "user_visible": False,
            "persisted": False,
            "status": "error",
            "error_type": type(error).__name__,
            "executed_calls": 0,
            "attempted_calls": structural.executed_calls,
            "pending_calls": structural.pending_calls,
        }
