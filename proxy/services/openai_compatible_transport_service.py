"""Provider-neutral HTTP transport for resolved model connections."""

from __future__ import annotations

from collections.abc import AsyncIterator, Mapping, Sequence
from dataclasses import dataclass, field
import json
from types import MappingProxyType
from typing import Any, Callable
from urllib.parse import urlsplit, urlunsplit

import httpx

from proxy.services.llm_transport_profile_service import assistant_delta_text
from proxy.services.model_reasoning_service import ReasoningOutput
from proxy.services.model_connection_contracts import CapabilityName, CapabilityState
from proxy.services.model_connection_resolver_service import ResolvedModelConnection
from proxy.services.model_connection_security_service import (
    ValidatedEndpoint,
    join_openai_path,
    validate_connected_peer,
)
from proxy.services.model_secret_service import EnvironmentSecretStore, ModelSecretError


class ModelTransportError(RuntimeError):
    """The exact resolved connection could not complete a safe request."""


PeerVerifier = Callable[[httpx.Response, ValidatedEndpoint], None]


async def read_chat_stream(response: httpx.Response, token_sink):
    """Read the legacy chat stream without accepting truncated text as a result."""
    response.raise_for_status()
    parts = []
    usage = {}
    finished = False
    try:
        async for line in response.aiter_lines():
            if not line.startswith("data:"):
                continue
            raw = line[5:].strip()
            if raw == "[DONE]":
                finished = True
                break
            try:
                chunk = json.loads(raw)
                choices = chunk.get("choices") or []
                choice = choices[0] if choices else {}
                piece = assistant_delta_text(choice.get("delta") or {})
            except (ValueError, TypeError, AttributeError, IndexError) as exc:
                raise ModelTransportError("UPSTREAM_STREAM_EVENT_INVALID") from exc
            if piece:
                parts.append(piece)
                await token_sink({"event": "token", "data": piece})
            finished = finished or bool(choice.get("finish_reason"))
            if chunk.get("usage"):
                usage = chunk["usage"]
    except (httpx.HTTPError, OSError) as exc:
        raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED") from exc
    if not finished:
        raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED")
    return "".join(parts), usage


@dataclass(frozen=True)
class InferenceRequest:
    messages: Sequence[Mapping[str, Any]]
    max_output_tokens: int
    temperature: float | None = None
    tools: Sequence[Mapping[str, Any]] = ()
    response_format: Mapping[str, Any] | None = None
    reasoning_enabled: bool = False

    def __post_init__(self) -> None:
        if not isinstance(self.reasoning_enabled, bool):
            raise ValueError("reasoning_enabled must be boolean")
        if self.max_output_tokens < 1:
            raise ValueError("max_output_tokens must be positive")
        object.__setattr__(self, "messages", tuple(dict(item) for item in self.messages))
        object.__setattr__(self, "tools", tuple(dict(item) for item in self.tools))
        if self.response_format is not None:
            object.__setattr__(self, "response_format", dict(self.response_format))


@dataclass(frozen=True)
class InferenceResponse:
    text: str
    tool_calls: tuple[Mapping[str, Any], ...]
    finish_reason: str
    usage: Mapping[str, int]
    model_id: str = ""


@dataclass(frozen=True)
class InferenceEvent:
    kind: str
    text: str = ""
    tool_calls: tuple[Mapping[str, Any], ...] = ()
    finish_reason: str = ""
    model_id: str = ""

    usage: Mapping[str, int] = field(default_factory=dict)

@dataclass(frozen=True)
class EmbeddingResponse:
    vectors: tuple[tuple[float, ...], ...]
    model_id: str
    usage: Mapping[str, int]


def _usage(value: object) -> Mapping[str, int]:
    if not isinstance(value, Mapping):
        return MappingProxyType({})
    result: dict[str, int] = {}
    for key, raw in value.items():
        if isinstance(raw, bool):
            continue
        try:
            parsed = int(raw)
        except (TypeError, ValueError):
            continue
        if parsed >= 0:
            result[str(key)] = parsed
    return MappingProxyType(result)


def _message_text(message: Mapping[str, Any]) -> str:
    content = message.get("content")
    if isinstance(content, str) and content:
        return content
    return assistant_delta_text(message)


class OpenAICompatibleTransport:
    def __init__(
        self,
        *,
        client: httpx.AsyncClient,
        secret_store: EnvironmentSecretStore,
        peer_verifier: PeerVerifier = validate_connected_peer,
        response_body_limit: int = 32_768,
        timeout: float = 120.0,
    ):
        if response_body_limit < 1:
            raise ValueError("response_body_limit must be positive")
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        self.client = client
        self.secret_store = secret_store
        self.peer_verifier = peer_verifier
        self.response_body_limit = response_body_limit
        self.timeout = timeout

    @staticmethod
    def _require(connection: ResolvedModelConnection, capability: CapabilityName) -> None:
        observation = connection.capability_snapshot.observation(capability)
        if observation.state is not CapabilityState.SUPPORTED:
            raise ModelTransportError(f"CAPABILITY_REQUIRED: {capability.value}")
        if observation.evidence_source in {"template_default", "unavailable"}:
            raise ModelTransportError(
                f"CAPABILITY_EVIDENCE_INSUFFICIENT: {capability.value}"
            )

    def _headers(self, connection: ResolvedModelConnection) -> dict[str, str]:
        headers = {"Accept": "application/json", "Content-Type": "application/json"}
        try:
            secret = self.secret_store.resolve(connection.secret_ref)
        except ModelSecretError as exc:
            raise ModelTransportError("CONNECTION_SECRET_MISSING") from exc
        if secret is not None:
            headers["Authorization"] = f"Bearer {secret.reveal()}"
        return headers

    @staticmethod
    def _output_field(connection: ResolvedModelConnection) -> str:
        return connection.capability_snapshot.transport_options.get(
            "max_output_field", "max_tokens"
        )

    def _chat_body(
        self,
        connection: ResolvedModelConnection,
        request: InferenceRequest,
        *,
        stream: bool,
    ) -> dict[str, Any]:
        messages = [dict(item) for item in request.messages]
        system_messages = [item for item in messages if item.get("role") == "system"]
        conversation = [item for item in messages if item.get("role") != "system"]
        if system_messages and all(isinstance(item.get("content"), str) for item in system_messages):
            system_messages = [{
                **system_messages[0],
                "content": "\n\n".join(str(item["content"]) for item in system_messages),
            }]
        body: dict[str, Any] = {
            "model": connection.model_id,
            "messages": system_messages + conversation,
            self._output_field(connection): request.max_output_tokens,
        }
        supports_local_options = connection.locality.value != "remote"
        if request.reasoning_enabled and not supports_local_options:
            raise ModelTransportError("REASONING_MODE_UNSUPPORTED")
        if supports_local_options:
            body["chat_template_kwargs"] = {"enable_thinking": request.reasoning_enabled}
        if request.temperature is not None:
            body["temperature"] = request.temperature
        if request.tools:
            self._require(connection, CapabilityName.TOOLS)
            body["tools"] = [dict(item) for item in request.tools]
        if request.response_format is not None:
            self._require(connection, CapabilityName.STRUCTURED_OUTPUT)
            body["response_format"] = dict(request.response_format)
        if stream:
            body["stream"] = True
        return body

    @staticmethod
    def _native_chat_url(connection: ResolvedModelConnection) -> str:
        parsed = urlsplit(connection.endpoint.canonical_base_url)
        path = parsed.path.rstrip("/")
        for suffix in ("/api/v1", "/v1"):
            if path.endswith(suffix):
                path = path[: -len(suffix)]
                break
        return urlunsplit((parsed.scheme, parsed.netloc, f"{path}/api/chat", "", ""))

    @staticmethod
    def _native_chat_body(
        connection: ResolvedModelConnection,
        request: InferenceRequest,
    ) -> dict[str, Any]:
        messages = [dict(item) for item in request.messages]
        for message in messages:
            if isinstance(message.get("content"), list):
                parts = message["content"]
                message["content"] = "\n".join(str(part.get("text") or "") for part in parts if part.get("type") == "text")
                images = []
                for part in parts:
                    if part.get("type") == "image_url":
                        url = str((part.get("image_url") or {}).get("url") or "")
                        if not url.startswith("data:image/") or ";base64," not in url:
                            raise ValueError("Native image requests require inline image bytes")
                        images.append(url.split(";base64,", 1)[1])
                if images:
                    message["images"] = images
        options: dict[str, Any] = {
            "num_predict": request.max_output_tokens,
            "num_ctx": int(connection.effective_preset.input_token_limit),
        }
        if request.temperature is not None:
            options["temperature"] = request.temperature
        body: dict[str, Any] = {
            "model": connection.model_id,
            "messages": messages,
            "think": request.reasoning_enabled,
            # Native chat sends one NDJSON event per generated fragment.
            # Consuming it keeps a long local generation alive while complete()
            # still returns the exact concatenated model answer as one value.
            "stream": True,
            "options": options,
        }
        if request.tools:
            body["tools"] = [dict(item) for item in request.tools]
        if request.response_format is not None:
            response_format = dict(request.response_format)
            if response_format.get("type") == "json_schema":
                json_schema = response_format.get("json_schema") or {}
                body["format"] = dict(json_schema.get("schema") or {})
            elif response_format.get("type") == "json_object":
                body["format"] = "json"
        return body

    async def _complete_native_chat(
        self,
        connection: ResolvedModelConnection,
        request: InferenceRequest,
        token_sink=None,
    ) -> InferenceResponse:
        if request.tools:
            self._require(connection, CapabilityName.TOOLS)
        if request.response_format is not None:
            self._require(connection, CapabilityName.STRUCTURED_OUTPUT)
        response = await self._open(
            connection,
            url=self._native_chat_url(connection),
            body=self._native_chat_body(connection, request),
        )
        try:
            if not 200 <= response.status_code < 300:
                await self._read_bounded(response)
                raise ModelTransportError(f"UPSTREAM_HTTP_ERROR: {response.status_code}")

            semantic_size = 0
            event_count = 0
            event_count_limit = max(256, request.max_output_tokens * 8)
            event_wire_limit = max(1024, self.response_body_limit * 8)
            saw_event = False
            finished = False
            reasoning_output = ReasoningOutput(request.reasoning_enabled)
            thinking_reported = False
            text_parts: list[str] = []
            tool_calls: list[Mapping[str, Any]] = []
            prompt_tokens = 0
            completion_tokens = 0
            finish_reason = ""
            model_id = connection.model_id
            if request.reasoning_enabled and token_sink is not None:
                await token_sink({"event": "progress", "data": {"stage": "thinking", "label": "Модель думает"}})
                thinking_reported = True
            async for line in response.aiter_lines():
                event_count += 1
                if event_count > event_count_limit or len(line.encode("utf-8")) > event_wire_limit:
                    raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
                if not line.strip():
                    continue
                try:
                    payload = json.loads(line)
                    message = payload["message"]
                    if not isinstance(message, Mapping):
                        raise TypeError("message must be a mapping")
                    prompt_tokens = int(payload.get("prompt_eval_count") or prompt_tokens)
                    completion_tokens = int(payload.get("eval_count") or completion_tokens)
                except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
                    raise ModelTransportError("UPSTREAM_RESPONSE_INVALID") from exc
                saw_event = True
                model_id = str(payload.get("model") or model_id)
                thought = message.get("thinking") or message.get("reasoning_content") or ""
                if thought:
                    reasoning_output.separate()
                    semantic_size += len(str(thought).encode("utf-8"))
                    if semantic_size > self.response_body_limit:
                        raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
                    if token_sink is not None and not thinking_reported:
                        await token_sink({"event": "progress", "data": {"stage": "thinking", "label": "Модель думает"}})
                        thinking_reported = True
                content = message.get("content")
                if isinstance(content, str) and content:
                    semantic_size += len(content.encode("utf-8"))
                    if semantic_size > self.response_body_limit:
                        raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
                    content = reasoning_output.feed(content)
                    text_parts.append(content)
                    if token_sink is not None and content:
                        await token_sink({"event": "token", "data": content})
                tool_calls_raw = message.get("tool_calls") or ()
                if not isinstance(tool_calls_raw, Sequence) or isinstance(tool_calls_raw, str):
                    raise ModelTransportError("UPSTREAM_TOOL_CALLS_INVALID")
                semantic_size += len(
                    json.dumps(tool_calls_raw, ensure_ascii=False).encode("utf-8")
                ) if tool_calls_raw else 0
                if semantic_size > self.response_body_limit:
                    raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
                tool_calls.extend(
                    MappingProxyType(dict(item))
                    for item in tool_calls_raw
                    if isinstance(item, Mapping)
                )
                finish_reason = str(payload.get("done_reason") or finish_reason)
                if payload.get("done") is True:
                    finished = True
                    break
            if saw_event and not finished:
                raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED")
            if not saw_event:
                raise ModelTransportError("UPSTREAM_RESPONSE_INVALID")
            try:
                tail = reasoning_output.finish(finish_reason)
            except ValueError as error:
                raise ModelTransportError(str(error)) from error
            text_parts.append(tail)
            if tail and token_sink is not None:
                await token_sink({"event": "token", "data": tail})
            return InferenceResponse(
                text="".join(text_parts),
                tool_calls=tuple(tool_calls),
                finish_reason=finish_reason,
                usage=MappingProxyType(
                    {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": prompt_tokens + completion_tokens,
                    }
                ),
                model_id=model_id,
            )
        finally:
            await response.aclose()

    @staticmethod
    def _fold_system_into_user(body: Mapping[str, Any]) -> dict[str, Any]:
        updated = dict(body)
        messages = [dict(item) for item in body.get("messages", ()) if isinstance(item, Mapping)]
        system_text = "\n\n".join(
            str(item.get("content") or "")
            for item in messages
            if item.get("role") == "system"
        ).strip()
        conversation = [item for item in messages if item.get("role") != "system"]
        if system_text:
            for item in conversation:
                if item.get("role") == "user" and isinstance(item.get("content"), str):
                    item["content"] = f"{system_text}\n\n{item['content']}"
                    break
            else:
                conversation.insert(0, {"role": "user", "content": system_text})
        updated["messages"] = conversation
        return updated

    async def _open(
        self,
        connection: ResolvedModelConnection,
        *,
        url: str,
        body: Mapping[str, Any],
    ) -> httpx.Response:
        request = self.client.build_request(
            "POST",
            url,
            headers=self._headers(connection),
            json=dict(body),
            timeout=self.timeout,
        )
        try:
            response = await self.client.send(request, stream=True, follow_redirects=False)
        except (httpx.HTTPError, OSError) as exc:
            raise ModelTransportError(f"UPSTREAM_REQUEST_FAILED: {type(exc).__name__}") from exc
        if 300 <= response.status_code < 400:
            await response.aclose()
            raise ModelTransportError("UPSTREAM_REDIRECT_REJECTED")
        try:
            self.peer_verifier(response, connection.endpoint)
        except (ValueError, OSError) as exc:
            await response.aclose()
            raise ModelTransportError(str(exc)) from exc
        return response

    async def _read_bounded(self, response: httpx.Response) -> bytes:
        chunks: list[bytes] = []
        size = 0
        async for chunk in response.aiter_bytes():
            size += len(chunk)
            if size > self.response_body_limit:
                raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
            chunks.append(chunk)
        return b"".join(chunks)

    async def complete(
        self,
        connection: ResolvedModelConnection,
        request: InferenceRequest,
        token_sink=None,
    ) -> InferenceResponse:
        self._require(connection, CapabilityName.CHAT_COMPLETIONS)
        if (
            connection.capability_snapshot.transport_options.get("chat_protocol")
            == "native_chat_v1"
        ):
            return await self._complete_native_chat(connection, request, token_sink=token_sink)
        if token_sink is not None:
            observation = connection.capability_snapshot.observation(CapabilityName.STREAMING)
            if observation.state is CapabilityState.SUPPORTED and observation.evidence_source not in {"template_default", "unavailable"}:
                from proxy.services.model_streaming_service import collect_stream
                return await collect_stream(self.stream(connection, request), token_sink)
            await token_sink({"event": "progress", "data": {"label": "Модель готовит ответ целиком — поток не поддерживается"}})
        body = self._chat_body(connection, request, stream=False)
        response = await self._open(
            connection,
            url=join_openai_path(connection.endpoint, "/chat/completions"),
            body=body,
        )
        try:
            raw = await self._read_bounded(response)
            if (
                response.status_code == 502
                and b"System message must be at the beginning" in raw
                and any(item.get("role") == "system" for item in body.get("messages", ()))
            ):
                await response.aclose()
                response = await self._open(
                    connection,
                    url=join_openai_path(connection.endpoint, "/chat/completions"),
                    body=self._fold_system_into_user(body),
                )
                raw = await self._read_bounded(response)
            if not 200 <= response.status_code < 300:
                raise ModelTransportError(f"UPSTREAM_HTTP_ERROR: {response.status_code}")
            try:
                payload = json.loads(raw)
                choice = payload["choices"][0]
                message = choice["message"]
            except (KeyError, IndexError, TypeError, json.JSONDecodeError) as exc:
                raise ModelTransportError("UPSTREAM_RESPONSE_INVALID") from exc
            tool_calls_raw = message.get("tool_calls") or ()
            if not isinstance(tool_calls_raw, Sequence) or isinstance(tool_calls_raw, str):
                raise ModelTransportError("UPSTREAM_TOOL_CALLS_INVALID")
            tool_calls = tuple(
                MappingProxyType(dict(item)) for item in tool_calls_raw if isinstance(item, Mapping)
            )
            reasoning_output = ReasoningOutput(request.reasoning_enabled)
            if message.get("reasoning_content") or message.get("reasoning"):
                reasoning_output.separate()
            text = reasoning_output.feed(_message_text(message))
            try:
                text += reasoning_output.finish(str(choice.get("finish_reason") or ""))
            except ValueError as error:
                raise ModelTransportError(str(error)) from error
            return InferenceResponse(
                text=text,
                tool_calls=tool_calls,
                finish_reason=str(choice.get("finish_reason") or ""),
                usage=_usage(payload.get("usage")),
                model_id=str(payload.get("model") or ""),
            )
        finally:
            await response.aclose()

    async def stream(
        self,
        connection: ResolvedModelConnection,
        request: InferenceRequest,
    ) -> AsyncIterator[InferenceEvent]:
        self._require(connection, CapabilityName.CHAT_COMPLETIONS)
        self._require(connection, CapabilityName.STREAMING)
        response = await self._open(
            connection,
            url=join_openai_path(connection.endpoint, "/chat/completions"),
            body=self._chat_body(connection, request, stream=True),
        )
        reasoning_output = ReasoningOutput(request.reasoning_enabled)
        thinking_reported = False
        consumed = 0
        events = 0
        finished = False
        observed_model_id = ""
        try:
            if not 200 <= response.status_code < 300:
                await self._read_bounded(response)
                raise ModelTransportError(f"UPSTREAM_HTTP_ERROR: {response.status_code}")
            if request.reasoning_enabled:
                yield InferenceEvent(kind="reasoning", model_id=observed_model_id)
                thinking_reported = True
            async for line in response.aiter_lines():
                events += 1
                if events > max(256, request.max_output_tokens * 8) or len(line.encode("utf-8")) > self.response_body_limit * 8:
                    raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
                if not line.startswith("data:"):
                    continue
                raw = line.removeprefix("data:").strip()
                if raw == "[DONE]":
                    if not finished:
                        try:
                            tail = reasoning_output.finish("")
                        except ValueError as error:
                            raise ModelTransportError(str(error)) from error
                        if tail:
                            yield InferenceEvent(kind="text_delta", text=tail, model_id=observed_model_id)
                    if not finished:
                        yield InferenceEvent(kind="finish", model_id=observed_model_id)
                    finished = True
                    break
                try:
                    payload = json.loads(raw)
                    if isinstance(payload.get("usage"), Mapping):
                        yield InferenceEvent(kind="usage", usage=_usage(payload["usage"]))
                        if not payload.get("choices"):
                            continue
                    choice = payload["choices"][0]
                    delta = choice.get("delta") or {}
                except (KeyError, IndexError, TypeError, json.JSONDecodeError) as exc:
                    raise ModelTransportError("UPSTREAM_STREAM_EVENT_INVALID") from exc
                observed_model_id = str(payload.get("model") or observed_model_id)
                thought = delta.get("reasoning") or delta.get("reasoning_content") or ""
                if thought:
                    reasoning_output.separate()
                    if not thinking_reported:
                        yield InferenceEvent(kind="reasoning", model_id=observed_model_id)
                        thinking_reported = True
                raw_text = assistant_delta_text(delta)
                text = reasoning_output.feed(raw_text)
                consumed += len(str(thought).encode("utf-8")) + len(raw_text.encode("utf-8")) + len(json.dumps(delta.get("tool_calls") or [], ensure_ascii=False).encode("utf-8"))
                if consumed > self.response_body_limit:
                    raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
                if text:
                    yield InferenceEvent(
                        kind="text_delta",
                        text=text,
                        model_id=observed_model_id,
                    )
                tool_calls_raw = delta.get("tool_calls") or ()
                if tool_calls_raw:
                    if not isinstance(tool_calls_raw, Sequence) or isinstance(tool_calls_raw, str):
                        raise ModelTransportError("UPSTREAM_TOOL_CALLS_INVALID")
                    yield InferenceEvent(
                        kind="tool_delta",
                        tool_calls=tuple(
                            MappingProxyType(dict(item))
                            for item in tool_calls_raw
                            if isinstance(item, Mapping)
                        ),
                        model_id=observed_model_id,
                    )
                finish_reason = str(choice.get("finish_reason") or "")
                if finish_reason:
                    try:
                        tail = reasoning_output.finish(finish_reason)
                    except ValueError as error:
                        raise ModelTransportError(str(error)) from error
                    if tail:
                        yield InferenceEvent(kind="text_delta", text=tail, model_id=observed_model_id)
                    finished = True
                    yield InferenceEvent(
                        kind="finish",
                        finish_reason=finish_reason,
                        model_id=observed_model_id,
                    )
            if not finished:
                raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED")
        except (httpx.HTTPError, OSError) as exc:
            raise ModelTransportError("UPSTREAM_STREAM_INTERRUPTED") from exc
        finally:
            await response.aclose()

    async def embed(
        self,
        connection: ResolvedModelConnection,
        inputs: Sequence[str],
    ) -> EmbeddingResponse:
        self._require(connection, CapabilityName.EMBEDDINGS)
        normalized_inputs = tuple(str(item) for item in inputs)
        if not normalized_inputs:
            raise ValueError("inputs must not be empty")
        response = await self._open(
            connection,
            url=join_openai_path(connection.endpoint, "/embeddings"),
            body={"model": connection.model_id, "input": list(normalized_inputs)},
        )
        try:
            raw = await self._read_bounded(response)
            if not 200 <= response.status_code < 300:
                raise ModelTransportError(f"UPSTREAM_HTTP_ERROR: {response.status_code}")
            try:
                payload = json.loads(raw)
                rows = sorted(payload["data"], key=lambda item: int(item["index"]))
                vectors = tuple(
                    tuple(float(value) for value in item["embedding"]) for item in rows
                )
            except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
                raise ModelTransportError("UPSTREAM_EMBEDDING_RESPONSE_INVALID") from exc
            if len(vectors) != len(normalized_inputs):
                raise ModelTransportError("UPSTREAM_EMBEDDING_COUNT_MISMATCH")
            return EmbeddingResponse(
                vectors=vectors,
                model_id=str(payload.get("model") or connection.model_id),
                usage=_usage(payload.get("usage")),
            )
        finally:
            await response.aclose()
