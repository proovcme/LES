from __future__ import annotations

import proxy.services.dataset_parse_service as ds_dataset_parse_service

from types import SimpleNamespace

import httpx
import pytest

import backend.qdrant_adapter as qdrant_adapter
import backend.embedding_client as embedding_client
from backend.interface import EmbeddingContractError
from backend.qdrant_adapter import EmbedClient
from proxy.services.model_connection_contracts import CapabilityName, ConnectionRole
from proxy.services.model_connection_resolver_service import ModelConnectionResolutionError
from proxy.services.openai_compatible_transport_service import EmbeddingResponse, ModelTransportError


class Resolver:
    def __init__(self, resolved=None, error=None):
        self.resolved = resolved
        self.error = error
        self.calls = []

    def resolve(self, role, *, required_capabilities=frozenset()):
        self.calls.append((role, required_capabilities))
        if self.error is not None:
            raise self.error
        return self.resolved


class Transport:
    def __init__(self, response):
        self.response = response
        self.calls = []

    async def embed(self, connection, inputs):
        self.calls.append((connection.revision_id, tuple(inputs)))
        return self.response


def _response(vectors, model="embed-model"):
    return EmbeddingResponse(
        vectors=tuple(tuple(row) for row in vectors),
        model_id=model,
        usage={},
    )


def test_active_embed_client_uses_exact_embeddings_binding() -> None:
    resolved = SimpleNamespace(revision_id="conn:embed:r3")
    resolver = Resolver(resolved)
    transport = Transport(_response([[0.1, 0.2]]))
    client = EmbedClient(
        "http://legacy",
        model="embed-model",
        connection_mode="active",
        connection_resolver=resolver,
        connection_transport=transport,
    )

    assert client.encode_sync(["duct"]) == [[0.1, 0.2]]
    assert resolver.calls == [
        (ConnectionRole.EMBEDDINGS, frozenset({CapabilityName.EMBEDDINGS}))
    ]
    assert transport.calls == [("conn:embed:r3", ("duct",))]


def test_shadow_embedding_uses_legacy_once_and_never_calls_candidate(monkeypatch) -> None:
    legacy_calls = []

    def post(_url, *, json, timeout):
        legacy_calls.append(tuple(json["input"]))
        return httpx.Response(
            200,
            json={
                "model": "embed-model",
                "data": [{"index": 0, "embedding": [1.0]}],
            },
        )

    monkeypatch.setattr(embedding_client.httpx, "post", post)
    resolver = Resolver(SimpleNamespace(revision_id="conn:candidate:r1"))
    transport = Transport(_response([[9.0]]))
    client = EmbedClient(
        "http://legacy",
        model="embed-model",
        backend="ollama",
        connection_mode="shadow",
        connection_resolver=resolver,
        connection_transport=transport,
    )

    assert client.encode_sync(["valve"]) == [[1.0]]
    assert len(legacy_calls) == 1
    assert len(resolver.calls) == 1
    assert transport.calls == []


def test_active_embedding_never_substitutes_answer_or_fallback() -> None:
    resolver = Resolver(error=ModelConnectionResolutionError("ROLE_BINDING_MISSING: embeddings"))
    transport = Transport(_response([[9.0]]))
    client = EmbedClient(
        "http://legacy",
        model="embed-model",
        connection_mode="active",
        connection_resolver=resolver,
        connection_transport=transport,
    )

    with pytest.raises(ModelConnectionResolutionError, match="ROLE_BINDING_MISSING: embeddings"):
        client.encode_sync(["valve"])

    assert resolver.calls == [
        (ConnectionRole.EMBEDDINGS, frozenset({CapabilityName.EMBEDDINGS}))
    ]
    assert transport.calls == []


@pytest.mark.asyncio
async def test_active_query_preserves_instruction_and_batch_order(monkeypatch) -> None:
    monkeypatch.setenv("LES_EMBED_PROFILE", "qwen")
    monkeypatch.setenv("RAG_QUERY_EMBEDDING_MODE", "qwen-retrieval-v1")
    resolver = Resolver(SimpleNamespace(revision_id="conn:embed:r1"))
    transport = Transport(_response([[1.0, 1.1], [2.0, 2.1]]))
    client = EmbedClient(
        "http://legacy",
        model="embed-model",
        connection_mode="active",
        connection_resolver=resolver,
        connection_transport=transport,
    )

    vectors = await client.encode_async(["first", "second"], query=True)

    assert vectors == [[1.0, 1.1], [2.0, 2.1]]
    sent = transport.calls[0][1]
    assert sent[0].startswith("Instruct: Given a search query")
    assert sent[0].endswith("Query: first")
    assert sent[1].endswith("Query: second")


def test_active_embedding_rejects_observed_model_and_dimension_drift() -> None:
    resolved = SimpleNamespace(revision_id="conn:embed:r1")
    wrong_model = EmbedClient(
        "http://legacy",
        model="embed-model",
        connection_mode="active",
        connection_resolver=Resolver(resolved),
        connection_transport=Transport(_response([[1.0]], model="other-model")),
    )
    with pytest.raises(EmbeddingContractError, match="embedding contract mismatch"):
        wrong_model.encode_sync(["one"])

    mixed_dimensions = EmbedClient(
        "http://legacy",
        model="embed-model",
        connection_mode="active",
        connection_resolver=Resolver(resolved),
        connection_transport=Transport(_response([[1.0], [2.0, 3.0]])),
    )
    with pytest.raises(EmbeddingContractError, match="embedding dimension mismatch"):
        mixed_dimensions.encode_sync(["one", "two"])


@pytest.mark.parametrize("response_capacity", [16, 3])
def test_document_embeddings_are_bounded_ordered_and_use_one_binding(response_capacity):
    resolver = Resolver(SimpleNamespace(revision_id="conn:embed:r1"))
    calls = []

    class BoundedTransport:
        async def embed(self, connection, inputs):
            calls.append((connection.revision_id, tuple(inputs)))
            if len(inputs) > response_capacity:
                raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
            return _response([[float(text), 0.0] for text in inputs])

    client = EmbedClient("http://unused", model="embed-model", connection_mode="active",
                         connection_resolver=resolver, connection_transport=BoundedTransport())
    assert client.encode_sync([str(i) for i in range(53)]) == [[float(i), 0.0] for i in range(53)]
    assert len(resolver.calls) == 1
    assert max(len(inputs) for _, inputs in calls) <= 16
    assert {revision for revision, _ in calls} == {"conn:embed:r1"}


@pytest.mark.parametrize("capacity", [16, 3])
def test_dimension_drift_between_sub_batches_is_rejected(capacity):
    class DriftingTransport:
        async def embed(self, connection, inputs):
            if len(inputs) > capacity:
                raise ModelTransportError("UPSTREAM_RESPONSE_TOO_LARGE")
            dimensions = 2 if inputs[0] == "0" else 3
            return _response([[1.0] * dimensions for _ in inputs])

    client = EmbedClient("http://unused", model="embed-model", connection_mode="active",
                         connection_resolver=Resolver(SimpleNamespace()), connection_transport=DriftingTransport())
    with pytest.raises(EmbeddingContractError, match="dimension mismatch"):
        client.encode_sync([str(i) for i in range(32)])


@pytest.mark.parametrize("error,count", [("UPSTREAM_RESPONSE_TOO_LARGE", 1), ("UPSTREAM_TIMEOUT", 16)])
def test_failed_embedding_batch_never_returns_partial_vectors_or_retries_other_errors(error, count):
    calls = []

    class FailingTransport:
        async def embed(self, connection, inputs):
            calls.append(tuple(inputs))
            if inputs[0] == "0":
                return _response([[1.0] for _ in inputs])
            raise ModelTransportError(error)

    client = EmbedClient("http://unused", model="embed-model", connection_mode="active",
                         connection_resolver=Resolver(SimpleNamespace()), connection_transport=FailingTransport())
    with pytest.raises(ModelTransportError, match=error):
        client.encode_sync([str(i) for i in range(16 + count)])
    assert len(calls) == 2


@pytest.mark.parametrize("light,expected", [(True,"active"), (False,"legacy")])
def test_production_adapter_wires_both_embedders_to_explicit_role(tmp_path, monkeypatch, light, expected):
    from backend import product_edition
    monkeypatch.setattr(product_edition, "is_light", lambda: light)
    monkeypatch.setattr(qdrant_adapter.support, "MetaDB", lambda: SimpleNamespace(ensure_system_datasets=lambda:None,
        requeue_repairable_errors=lambda **kw:{"repaired_files":0}, recover_interrupted_parsing=lambda:0))
    monkeypatch.setattr(qdrant_adapter.qdrant_client, "AsyncQdrantClient", lambda **kw:None)
    calls=[]
    monkeypatch.setattr(qdrant_adapter.support, "EmbedClient", lambda *args, **kw: calls.append(kw) or SimpleNamespace())
    adapter=qdrant_adapter.QdrantLlamaIndexAdapter("http://127.0.0.1:6333", "http://127.0.0.1:11434", "embed-model", content_dir=tmp_path)
    assert len(calls)==2
    assert all(call["connection_mode"]==expected for call in calls)


@pytest.mark.asyncio
async def test_light_admission_blocks_before_health_or_model_call(monkeypatch):
    from backend import product_edition
    from proxy.routers import datasets
    from fastapi import HTTPException
    monkeypatch.setattr(product_edition, "is_light", lambda:True)
    def missing():
        raise ModelConnectionResolutionError("ROLE_BINDING_MISSING: embeddings")
    backend=SimpleNamespace(embed_parse=SimpleNamespace(connection_mode="active", _resolve_embedding_connection=missing))
    with pytest.raises(HTTPException) as error:
        await ds_dataset_parse_service.assert_parse_admission(SimpleNamespace(backend=backend))
    assert error.value.status_code==409
    assert "назначьте" in error.value.detail


@pytest.fixture(autouse=True)
def isolated_qdrant_identity(monkeypatch):
    monkeypatch.setenv("LES_LIGHT_QDRANT_URL", "http://127.0.0.1:6333")
    monkeypatch.setenv("LES_LIGHT_QDRANT_API_KEY", "synthetic-unit-key")
