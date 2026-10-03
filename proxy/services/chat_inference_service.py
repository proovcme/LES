"""Explicit provider context, capability refresh and model connection resolution."""
from __future__ import annotations
import logging
import os
from contextvars import ContextVar
from dataclasses import dataclass
from typing import Any
import httpx
from proxy.config import ENV_PATH
from proxy.services.chat_provider_session_service import ChatProviderConfig
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_capability_service import CapabilityProbe
from proxy.services.model_connection_contracts import CapabilityName, ConnectionRole
from proxy.services.model_connection_resolver_service import (
    ModelConnectionResolutionError,
    ModelConnectionResolver,
)
from proxy.services.model_secret_service import EnvironmentSecretStore
from backend.inference.routing import estimate_cost_usd, load_price_table_from_env
from proxy.local_model_registry import DEFAULT_LOCAL_MLX_MODEL

logger = logging.getLogger(__name__)

DEFAULT_OPENAI_MODEL = "gpt-5.4"


_REQUEST_LLM_RUNTIME: ContextVar[Any | None] = ContextVar("request_llm_runtime", default=None)


_REQUEST_CLOUD_CONSENT: ContextVar[bool | None] = ContextVar("request_cloud_consent", default=None)


def _env_float(name: str, default: float) -> float:
    try:
        return float(os.getenv(name, str(default)))
    except ValueError:
        return default


def _env_int(name: str, default: int) -> int:
    try:
        return int(os.getenv(name, str(default)))
    except ValueError:
        return default


def _env_bool(name: str, default: bool) -> bool:
    if name == "LES_CLOUD_CONSENT":
        request_consent = _REQUEST_CLOUD_CONSENT.get()
        if request_consent is not None:
            return request_consent
    raw = os.getenv(name)
    if raw is None:
        return default
    return raw.strip().lower() in {"1", "true", "yes", "on"}


@dataclass(frozen=True)
class LlmRuntime:
    provider: str
    base_url: str
    chat_url: str
    model: str
    api_key: str
    supports_validation: bool
    requires_cache_alignment: bool = False
    uses_native_chat: bool = False


def _join_openai_path(base_url: str, path: str) -> str:
    base = base_url.rstrip("/")
    if base.endswith("/v1") or base.endswith("/api/v1"):
        return f"{base}{path}"
    return f"{base}/v1{path}"


def _is_local_llm_url(base_url: str) -> bool:
    low = (base_url or "").strip().lower()
    return (
        low.startswith("http://127.")
        or low.startswith("http://localhost")
        or low.startswith("http://[::1]")
        or low.startswith("http://0.0.0.0")
    )


def _llm_runtime() -> LlmRuntime:
    request_runtime = _REQUEST_LLM_RUNTIME.get()
    if request_runtime is not None:
        return request_runtime
    provider = os.getenv("LES_LLM_PROVIDER", "mlx").strip().lower() or "mlx"
    if provider == "freetoken":
        base_url = os.getenv("FREETOKEN_BASE_URL", "http://127.0.0.1:1919/v1").strip()
        model = os.getenv("FREETOKEN_MODEL", "").strip() or os.getenv("LLM_MODEL", "")
        api_key = os.getenv("FREETOKEN_API_KEY", "").strip()
        return LlmRuntime(
            provider,
            base_url,
            _join_openai_path(base_url, "/chat/completions"),
            model,
            api_key,
            False,
            requires_cache_alignment=True,
        )
    if provider == "openrouter":
        base_url = os.getenv("OPENROUTER_BASE_URL", "https://openrouter.ai/api/v1").strip()
        model = os.getenv("OPENROUTER_MODEL", "").strip() or os.getenv("LLM_MODEL", "")
        api_key = os.getenv("OPENROUTER_API_KEY", "").strip()
        if not api_key and not _is_local_llm_url(base_url):
            return _mlx_runtime()
        return LlmRuntime(provider, base_url, _join_openai_path(base_url, "/chat/completions"), model, api_key, False)
    if provider in {"openai", "openai-compatible", "openai_compatible"}:
        base_url = os.getenv("OPENAI_BASE_URL", "").strip() or "https://api.openai.com/v1"
        model = os.getenv("OPENAI_MODEL", "").strip() or os.getenv("LES_DEFAULT_OPENAI_MODEL", DEFAULT_OPENAI_MODEL)
        api_key = os.getenv("OPENAI_API_KEY", "").strip()
        if not api_key and not _is_local_llm_url(base_url):
            return _mlx_runtime()
        return LlmRuntime(provider, base_url, _join_openai_path(base_url, "/chat/completions"), model, api_key, False)
    if provider == "ollama":
        base_url = os.getenv("OLLAMA_BASE_URL", os.getenv("OLLAMA_URL", "http://127.0.0.1:11434")).strip()
        model = os.getenv("OLLAMA_MODEL", "").strip() or os.getenv("LLM_MODEL", "")
        api_key = os.getenv("OLLAMA_API_KEY", "").strip()
        return LlmRuntime(
            provider,
            base_url,
            _join_openai_path(base_url, "/chat/completions"),
            model,
            api_key,
            False,
            uses_native_chat=True,
        )
    if provider == "lemonade":
        base_url = os.getenv("LEMONADE_BASE_URL", "http://127.0.0.1:13305/api/v1").strip()
        model = os.getenv("LEMONADE_MODEL", "").strip() or os.getenv("LLM_MODEL", "")
        api_key = os.getenv("LEMONADE_API_KEY", "lemonade").strip()
        return LlmRuntime(provider, base_url, _join_openai_path(base_url, "/chat/completions"), model, api_key, False)

    return _mlx_runtime()


def _mlx_runtime() -> LlmRuntime:
    """Локальный MLX-провайдер — он же fallback политики маршрутизации (W3.3)."""
    base_url = os.getenv("MLX_URL", "http://127.0.0.1:8080").strip()
    model = (
        os.getenv("LLM_MODEL", "").strip()
        or os.getenv("MLX_MODEL", "").strip()
        or DEFAULT_LOCAL_MLX_MODEL
    )
    return LlmRuntime("mlx", base_url, _join_openai_path(base_url, "/chat/completions"), model, "", True)


def _runtime_from_provider_config(config: ChatProviderConfig) -> LlmRuntime:
    if config.provider == "mlx":
        return _mlx_runtime()
    if config.provider == "openrouter":
        base_url = "https://openrouter.ai/api/v1"
    else:
        base_url = "https://api.openai.com/v1"
    return LlmRuntime(
        config.provider,
        base_url,
        _join_openai_path(base_url, "/chat/completions"),
        config.model,
        config.api_key,
        False,
    )


def model_connection_timeout() -> float:
    """One end-to-end window for an explicitly assigned answer connection."""
    try:
        return float(os.getenv("LES_MODEL_CONNECTION_TIMEOUT_SEC", 300.0))
    except (TypeError, ValueError):
        return 300.0


def _model_connection_resolver() -> tuple[ModelConnectionResolver, EnvironmentSecretStore]:
    secret_store = EnvironmentSecretStore(ENV_PATH)
    return ModelConnectionResolver(
        registry=ModelConnectionRegistry(),
        secret_store=secret_store,
        allow_private_http=True,
    ), secret_store


def _model_capability_probe(
    client: httpx.AsyncClient,
    secret_store: EnvironmentSecretStore,
) -> CapabilityProbe:
    return CapabilityProbe(
        client=client,
        secret_store=secret_store,
        allow_private_http=True,
    )


async def _refresh_stale_bound_model_capabilities(client: httpx.AsyncClient) -> None:
    from proxy.services.chat_runtime import get_chat_state
    from proxy.services.model_capability_refresh_service import refresh_bound_capabilities
    resolver, secret_store = _model_connection_resolver()
    if getattr(resolver, "registry", None) is None:
        return
    await refresh_bound_capabilities(resolver=resolver,
        probe=_model_capability_probe(client, secret_store), state=get_chat_state())


def _record_cloud_cost(state: "ChatRouterState", model: str, usage: dict[str, Any]) -> None:
    """Учёт расходов облака (токены → $) в метриках. Локальные вызовы сюда не идут."""
    prompt_tokens = int(usage.get("prompt_tokens", 0) or 0)
    completion_tokens = int(usage.get("completion_tokens", 0) or 0)
    cost = estimate_cost_usd(model, prompt_tokens, completion_tokens, load_price_table_from_env())
    metrics = state.chat_metrics
    metrics["cloud_requests"] = metrics.get("cloud_requests", 0) + 1
    metrics["cloud_prompt_tokens"] = metrics.get("cloud_prompt_tokens", 0) + prompt_tokens
    metrics["cloud_completion_tokens"] = metrics.get("cloud_completion_tokens", 0) + completion_tokens
    metrics["cloud_cost_usd"] = round(metrics.get("cloud_cost_usd", 0.0) + cost, 6)
    by_model = metrics.setdefault("cloud_cost_by_model", {})
    by_model[model] = round(by_model.get(model, 0.0) + cost, 6)


def chat_validation_enabled() -> bool:
    return os.getenv("CHAT_VALIDATION_ENABLED", "true").lower() in {"1", "true", "yes", "on"}


def resolve_required_answer(factory=None):
    """Resolve the explicit answer binding; promotion experiments are not a gate."""
    resolver, secrets = (factory or _model_connection_resolver)()
    return resolver, secrets, resolver.resolve(ConnectionRole.ANSWER)


async def run_bound_inference(runner, request, *, remote_allowed=True, token_sink=None):
    """The single generation entry point for both ordinary and document chats."""
    from proxy.services.canonical_route_service import CanonicalRouteMode

    async def reject_legacy(_request):
        raise RuntimeError("LEGACY_MODEL_CALL_FORBIDDEN_IN_ACTIVE_MODE")

    return await runner.complete(
        mode=CanonicalRouteMode.ACTIVE, request=request,
        legacy_complete=reject_legacy, remote_allowed=remote_allowed, token_sink=token_sink,
    )
