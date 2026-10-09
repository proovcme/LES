"""Embedding transport and response validation for explicit model connections."""
import asyncio
import copy
import hashlib
import logging
import math
import os
import re
import time
from concurrent.futures import ThreadPoolExecutor
from typing import Any, List
import httpx
from backend.interface import EmbeddingContractError
from backend.rag_config import prepare_query_for_embedding
from proxy.config import ENV_PATH
from proxy.services.model_connection_contracts import CapabilityName, ConnectionRole
from proxy.services.model_connection_registry_service import ModelConnectionRegistry
from proxy.services.model_connection_resolver_service import ModelConnectionResolver
from proxy.services.model_secret_service import EnvironmentSecretStore
from proxy.services.openai_compatible_transport_service import ModelTransportError, OpenAICompatibleTransport
logger = logging.getLogger(__name__)
EMBED_TIMEOUT = float(os.getenv("RAG_EMBED_TIMEOUT_SEC", "300"))
EMBED_BATCH_SIZE = 16
EMBED_RESPONSE_LIMIT = 1024 * 1024

class EmbedClient:
    """
    Тонкий клиент к /v1/embeddings MLX Host.
    Работает асинхронно и синхронно (для _sync_parse в threadpool).
    """
    def __init__(
        self,
        base_url: str,
        model: str = "bge-m3",
        *,
        backend: str | None = None,
        connection_mode: str = "legacy",
        connection_resolver: ModelConnectionResolver | None = None,
        connection_transport: OpenAICompatibleTransport | None = None,
    ):
        self.url   = f"{base_url.rstrip('/')}/v1/embeddings"
        self.model = model
        normalized_mode = str(connection_mode or "legacy").strip().lower()
        if normalized_mode not in {"legacy", "shadow", "active"}:
            raise ValueError("connection_mode must be legacy, shadow or active")
        self.connection_mode = normalized_mode
        self.connection_resolver = connection_resolver
        self.connection_transport = connection_transport
        self._connection_secret_store: EnvironmentSecretStore | None = None
        if self.connection_mode != "legacy" and self.connection_resolver is None:
            self._connection_secret_store = EnvironmentSecretStore(ENV_PATH)
            self.connection_resolver = ModelConnectionResolver(
                registry=ModelConnectionRegistry(),
                secret_store=self._connection_secret_store,
                allow_private_http=True,
            )
        if backend is not None:
            self.backend = str(backend).strip().lower()
        elif "11434" in base_url or "ollama" in base_url or os.getenv("EMBED_BACKEND") == "ollama":
            self.backend = "ollama"
        else:
            self.backend = os.getenv("EMBED_BACKEND", "sentence_transformers").strip().lower()

    def _resolve_embedding_connection(self):
        frozen = getattr(self, "_frozen_connection", None)
        if frozen is not None:
            return frozen
        if self.connection_resolver is None:
            raise RuntimeError("MODEL_CONNECTION_RESOLVER_REQUIRED")
        return self.connection_resolver.resolve(
            ConnectionRole.EMBEDDINGS,
            required_capabilities=frozenset({CapabilityName.EMBEDDINGS}),
        )

    def for_index(self, descriptor: dict[str, str]):
        """Freeze one binding for a whole document/query, including cache hits.

        An existing collection never silently changes vector space when a role
        is reassigned. A different model requires an explicit new index.
        """
        frozen = copy.copy(self)
        expected = descriptor["model_id"]
        if self._normalise_model_id(self.model) != self._normalise_model_id(expected):
            raise EmbeddingContractError("EMBEDDING_INDEX_MODEL_MISMATCH")
        if self.connection_mode == "active":
            connection = self._resolve_embedding_connection()
            if self._normalise_model_id(connection.model_id) != self._normalise_model_id(expected):
                raise EmbeddingContractError("EMBEDDING_INDEX_MODEL_MISMATCH")
            frozen._frozen_connection = connection
        frozen._index_vector_size = int(descriptor["vector_size"])
        return frozen

    def _shadow_compare(self) -> None:
        try:
            self._resolve_embedding_connection()
        except Exception as exc:
            logger.info("Embedding connection shadow comparison unavailable: %s", type(exc).__name__)

    def _vectors_from_connection_response(self, response: Any, expected_count: int) -> List[List[float]]:
        expected = self._normalise_model_id(self.model)
        actual_model = str(response.model_id or "").strip()
        actual = self._normalise_model_id(actual_model)
        if not expected or expected != actual:
            raise EmbeddingContractError(
                f"embedding contract mismatch: expected={self.model}, actual={actual_model}"
            )
        vectors = [[float(value) for value in row] for row in response.vectors]
        if len(vectors) != expected_count:
            raise EmbeddingContractError(
                f"embedding count mismatch: expected={expected_count}, actual={len(vectors)}"
            )
        dimensions = {len(row) for row in vectors}
        if not dimensions or 0 in dimensions or len(dimensions) != 1:
            raise EmbeddingContractError("embedding dimension mismatch")
        index_size = getattr(self, "_index_vector_size", None)
        if index_size is not None and dimensions != {index_size}:
            raise EmbeddingContractError("EMBEDDING_INDEX_DIMENSION_MISMATCH")
        if any(not math.isfinite(value) for row in vectors for value in row):
            raise EmbeddingContractError('embedding response contains non-finite values')
        return vectors

    async def _encode_connection_async(self, texts: List[str]) -> List[List[float]]:
        if not texts:
            return []
        connection = self._resolve_embedding_connection()
        if self.connection_transport is not None:
            return await self._encode_connection_batches(self.connection_transport, connection, texts)
        else:
            secret_store = self._connection_secret_store or EnvironmentSecretStore(ENV_PATH)
            async with httpx.AsyncClient(timeout=EMBED_TIMEOUT) as client:
                transport = OpenAICompatibleTransport(
                    client=client,
                    secret_store=secret_store,
                    timeout=EMBED_TIMEOUT,
                    # Vectors need more space than a short chat/probe response.
                    # This cap applies only to this embedding transport.
                    response_body_limit=EMBED_RESPONSE_LIMIT,
                )
                return await self._encode_connection_batches(transport, connection, texts)

    async def _encode_connection_batches(self, transport, connection, texts: List[str]) -> List[List[float]]:
        """Keep responses bounded without changing the frozen model binding.

        High-dimensional providers may need smaller batches. Only the explicit
        response-size error permits subdivision; all other failures propagate.
        No partial result is returned when any batch violates the contract.
        """
        async def encode_batch(batch):
            try:
                response = await transport.embed(connection, batch)
            except ModelTransportError as error:
                if str(error) != "UPSTREAM_RESPONSE_TOO_LARGE" or len(batch) == 1:
                    raise
                middle = len(batch) // 2
                return await encode_batch(batch[:middle]) + await encode_batch(batch[middle:])
            return self._vectors_from_connection_response(response, len(batch))

        vectors = []
        for offset in range(0, len(texts), EMBED_BATCH_SIZE):
            batch_vectors = await encode_batch(texts[offset:offset + EMBED_BATCH_SIZE])
            expected_dimension = len(vectors[0]) if vectors else len(batch_vectors[0])
            if any(len(row) != expected_dimension for row in batch_vectors):
                raise EmbeddingContractError("embedding dimension mismatch across batches")
            vectors.extend(batch_vectors)
        return vectors

    def _encode_connection_sync(self, texts: List[str]) -> List[List[float]]:
        def run() -> List[List[float]]:
            return asyncio.run(self._encode_connection_async(texts))

        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return run()
        with ThreadPoolExecutor(max_workers=1) as executor:
            return executor.submit(run).result()

    @staticmethod
    def _normalise_model_id(value: object) -> str:
        name = str(value or '').strip().casefold().removesuffix(':latest')
        # Known registry alias, not substring matching that accepts other models.
        aliases = {
            'baai/bge-m3': 'bge-m3',
            'qwen/qwen3-embedding-0.6b': 'qwen3-embedding-0.6b',
            'qwen/qwen3-embedding-4b': 'qwen3-embedding-4b',
            'qwen/qwen3-embedding-8b': 'qwen3-embedding-8b',
        }
        return aliases.get(name, name)

    def _vectors_from_response(self, payload: dict[str, Any]) -> List[List[float]]:
        """Validate the model that actually produced a response before using it.

        The OpenAI ``model`` request field is descriptive for the local MLX host:
        it does not select a model.  A host must therefore report the active
        ``embedding_model`` explicitly.  Missing or incompatible metadata is a
        safety failure, not a reason to score Qwen and BGE vectors together.
        """
        reported_embedding_model = str(payload.get("embedding_model") or "").strip()
        reported_openai_model = str(payload.get("model") or "").strip()
        # Ollama's OpenAI-compatible endpoint selects the requested model and
        # reports it in the standard ``model`` field.  LES-owned MLX/CoreML
        # hosts must keep reporting the stronger explicit contract fields.
        ollama_contract = (self.backend == "ollama" or "11434" in self.url or "ollama" in self.url) and bool(reported_openai_model)
        actual_model = reported_embedding_model or (reported_openai_model if ollama_contract else "")
        actual_backend = str(payload.get("embedding_backend") or "").strip().lower()
        if not actual_backend and ollama_contract:
            actual_backend = "ollama"
        expected = self._normalise_model_id(self.model)
        actual = self._normalise_model_id(actual_model)
        if not actual_model:
            raise EmbeddingContractError(
                f"embedding contract not reported by {self.url}; expected={self.model}"
            )
        if not expected or expected != actual:
            raise EmbeddingContractError(
                f"embedding contract mismatch: expected={self.model}, actual={actual_model}"
            )
        expected_backend = self.backend
        if not actual_backend:
            raise EmbeddingContractError(
                f"embedding backend not reported by {self.url}; expected={expected_backend}"
            )
        if actual_backend != expected_backend:
            raise EmbeddingContractError(
                "embedding backend mismatch: "
                f"expected={expected_backend}, actual={actual_backend}"
            )
        data = payload.get("data") or []
        data.sort(key=lambda x: x["index"])
        return [d["embedding"] for d in data]

    @staticmethod
    def _response_detail(response: Any) -> str:
        try:
            payload = response.json()
        except Exception:
            payload = None
        if isinstance(payload, dict):
            detail = payload.get("error") or payload.get("detail") or payload.get("message")
            if detail:
                return " ".join(str(detail).split())[:500]
        return " ".join(str(getattr(response, "text", "") or "").split())[:500]

    def _encode_sync_resilient(self, texts: List[str]) -> List[List[float]]:
        import httpx as _httpx

        try:
            attempts = max(1, int(os.getenv("RAG_EMBED_RETRY_ATTEMPTS", "3")))
        except ValueError:
            attempts = 3
        try:
            delay = max(0.0, float(os.getenv("RAG_EMBED_RETRY_DELAY_SEC", "0.35")))
        except ValueError:
            delay = 0.35

        response = None
        request_error: Exception | None = None
        retryable = {400, 408, 409, 425, 429, 500, 502, 503, 504}
        for attempt in range(1, attempts + 1):
            try:
                response = _httpx.post(
                    self.url,
                    json={"model": self.model, "input": texts},
                    timeout=EMBED_TIMEOUT,
                )
                if response.status_code < 400:
                    return self._vectors_from_response(response.json())
                request_error = None
                if response.status_code not in retryable:
                    break
            except _httpx.RequestError as error:
                request_error = error
                response = None
            if attempt < attempts:
                time.sleep(delay * attempt)

        # Ollama can reject one batch transiently while the same inputs work in
        # smaller groups.  Split only after bounded retries; a bad item is then
        # isolated without discarding already valid document chunks.
        if len(texts) > 1 and (response is None or response.status_code in retryable):
            middle = max(1, len(texts) // 2)
            return self._encode_sync_resilient(texts[:middle]) + self._encode_sync_resilient(texts[middle:])

        text_hash = hashlib.sha256((texts[0] if texts else "").encode("utf-8", errors="ignore")).hexdigest()[:12]
        if response is not None:
            detail = self._response_detail(response) or "сервер не сообщил причину"
            raise RuntimeError(
                "Сервис поискового представления отклонил фрагмент "
                f"после {attempts} попыток: HTTP {response.status_code}; {detail}; "
                f"fragment={text_hash}"
            )
        raise RuntimeError(
            "Сервис поискового представления недоступен "
            f"после {attempts} попыток: {request_error}; fragment={text_hash}"
        )

    def encode_sync(self, texts: List[str]) -> List[List[float]]:
        """Синхронный parse-клиент с bounded retry и изоляцией плохого фрагмента."""
        if not texts:
            return []
        if self.connection_mode == "active":
            return self._encode_connection_sync(texts)
        if self.connection_mode == "shadow":
            self._shadow_compare()
        return self._encode_sync_resilient(texts)

    async def encode_async(self, texts: List[str], *, query: bool = False) -> List[List[float]]:
        """Асинхронный вариант для retrieve; query contract never touches documents."""
        payload_texts = [prepare_query_for_embedding(text) for text in texts] if query else texts
        if self.connection_mode == "active":
            return await self._encode_connection_async(payload_texts)
        if self.connection_mode == "shadow":
            self._shadow_compare()
        async with httpx.AsyncClient(timeout=30.0) as c:
            r = await c.post(
                self.url,
                json={"model": self.model, "input": payload_texts},
            )
            r.raise_for_status()
            return self._vectors_from_response(r.json())
