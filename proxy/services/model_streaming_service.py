"""Assemble a validated provider stream while delivering text immediately."""
from __future__ import annotations

from proxy.services.openai_compatible_transport_service import InferenceResponse, ModelTransportError


async def collect_stream(events, token_sink):
    parts, calls, usage = [], {}, {}
    finish_reason, model_id = "", ""
    finished = False
    try:
        async for event in events:
            model_id = event.model_id or model_id
            if event.kind == "text_delta":
                parts.append(event.text)
                await token_sink({"event": "token", "data": event.text})
            elif event.kind == "reasoning":
                await token_sink({"event": "progress", "data": {"stage": "thinking", "label": "Модель думает"}})
            elif event.kind == "tool_delta":
                for delta in event.tool_calls:
                    index = delta.get("index", 0)
                    if isinstance(index, bool) or not isinstance(index, int) or not 0 <= index < 256:
                        raise ModelTransportError("UPSTREAM_TOOL_CALLS_INVALID")
                    call = calls.setdefault(index, {"id": "", "type": "function", "function": {"name": "", "arguments": ""}})
                    if delta.get("id"):
                        call["id"] = str(delta["id"])
                    function = delta.get("function") or {}
                    for key in ("name", "arguments"):
                        value = function.get(key, "")
                        if not isinstance(value, str):
                            raise ModelTransportError("UPSTREAM_TOOL_CALLS_INVALID")
                        call["function"][key] += value
            elif event.kind == "usage":
                usage.update(event.usage)
            elif event.kind == "finish":
                finished = True
                finish_reason = event.finish_reason or finish_reason
    finally:
        close = getattr(events, "aclose", None)
        if close is not None:
            await close()
    if not finished:
        raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED")
    return InferenceResponse(
        text="".join(parts), tool_calls=tuple(calls[key] for key in sorted(calls)),
        finish_reason=finish_reason, usage=usage, model_id=model_id,
    )
