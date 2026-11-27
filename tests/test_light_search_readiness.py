"""Native index evidence must agree with the actual rendered catalog."""
from types import SimpleNamespace


from proxy.services import rag_readiness_service as readiness


def test_light_readiness_does_not_require_removed_estimate_modules(monkeypatch):
    class Index:
        def get_aliases(self):
            return SimpleNamespace(aliases=[])

        def collection_exists(self, name):
            return True

        def count(self, *args, **kwargs):
            return SimpleNamespace(count=2)

        def close(self):
            pass

    monkeypatch.setattr(readiness, "QdrantClient", lambda **kwargs: Index())
    monkeypatch.setenv("QDRANT_URL", "http://127.0.0.1:16339")
    monkeypatch.setenv("LES_LIGHT_QDRANT_URL", "http://127.0.0.1:16339")
    monkeypatch.setenv("LES_LIGHT_QDRANT_API_KEY", "synthetic-fixture-key")
    monkeypatch.setattr(readiness, "rag_collection_name", lambda: "les_rag")
    monkeypatch.setattr(readiness, "_source_chunks", lambda dataset_id: 2)
    monkeypatch.setattr(readiness, "_lexical_status", lambda *args: {})
    monkeypatch.setattr(readiness, "index_contract_status", lambda: {
        "compatible": True, "actual": {"point_embedding_fingerprint": "fixture"},
    })
    monkeypatch.setattr(readiness, "load_policy", lambda: {})
    monkeypatch.setattr(readiness, "load_status", lambda: {})
    result = readiness.rag_readiness(dataset_id="fixture", force=True)
    assert result["status"] == "ok"
    assert result["general"]["rrf_ready"] is True
    assert result["general"]["points"] == 2
    assert "smeta" not in result
