"""Assemble a validated provider stream while delivering text immediately."""
from __future__ import annotations

from backend import native_core
from proxy.services.openai_compatible_transport_service import InferenceResponse, ModelTransportError


async def collect_stream(events, token_sink):
    parts, calls, usage = [], {}, {}
    finish_reason, model_id = "", ""
    finished = False
    final_calls = ()
    native = None
    try:
        if native_core.supports("chat_stream"):
            native = native_core.ChatStream()
        async for event in events:
            if event.kind == 'progress':
                await _deliver(event, token_sink)
                continue
            if native is not None:
                frame = {"kind": event.kind, "model_id": event.model_id}
                if event.kind == "text_delta":
                    frame["text"] = event.text
                elif event.kind == "usage":
                    frame["usage"] = dict(event.usage)
                elif event.kind in {"tool_delta", "finish"}:
                    frame["tool_calls"] = [dict(call) for call in event.tool_calls]
                    frame["finish_reason"] = event.finish_reason
                native.submit(frame)
                await _deliver(event, token_sink)
                continue
            model_id = event.model_id or model_id
            if event.kind == "text_delta":
                parts.append(event.text)
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
                final_calls = event.tool_calls or final_calls
            await _deliver(event, token_sink)
        if native is not None:
            result = native.submit(None)
            return InferenceResponse(
                text=result["text"], tool_calls=tuple(result["tool_calls"]),
                finish_reason=result["finish_reason"], usage=result["usage"], model_id=result["model_id"],
            )
        if not finished:
            raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED")
        return InferenceResponse(
            text="".join(parts), tool_calls=final_calls or tuple(calls[key] for key in sorted(calls)),
            finish_reason=finish_reason, usage=usage, model_id=model_id,
        )
    except native_core.NativeCoreError as exc:
        code = "UPSTREAM_RESPONSE_TOO_LARGE" if str(exc) == "CORE_OUTPUT_LIMIT" else str(exc)
        raise ModelTransportError(code) from exc
    finally:
        if native is not None:
            native.close()
        close = getattr(events, "aclose", None)
        if close is not None:
            await close()


async def _deliver(event, token_sink):
    if token_sink is None:
        return
    if event.kind == "text_delta":
        await token_sink({"event": "token", "data": event.text})
    elif event.kind == "reasoning":
        await token_sink({"event": "progress", "data": {"stage": "thinking", "label": "Модель думает"}})
    elif event.kind == 'progress':
        await token_sink({'event': 'progress', 'data': {'label': event.text}})
