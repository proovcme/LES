import pytest

from backend.light_qdrant_connection import qdrant_client_options, qdrant_http_headers


def test_full_edition_cannot_reuse_light_qdrant_key(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "full")
    monkeypatch.setenv("LES_LIGHT_QDRANT_API_KEY", "test-key")
    with pytest.raises(ValueError, match="LES Light only"):
        qdrant_client_options("http://127.0.0.1:6333")


def test_light_key_is_scoped_to_exact_owned_server(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    monkeypatch.setenv("LES_LIGHT_QDRANT_API_KEY", "test-key")
    monkeypatch.setenv("LES_LIGHT_QDRANT_URL", "http://127.0.0.1:57123")
    assert qdrant_client_options("http://127.0.0.1:57123/") == {"api_key": "test-key"}
    assert qdrant_http_headers("http://127.0.0.1:57123") == {"api-key": "test-key"}
    for url in ("http://127.0.0.1:6333", "https://example.com", "http://127.0.0.1:57123/foreign"):
        with pytest.raises(RuntimeError):
            qdrant_client_options(url)


def test_light_missing_launcher_state_never_falls_back_to_full_qdrant(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    monkeypatch.delenv("LES_LIGHT_QDRANT_API_KEY", raising=False)
    monkeypatch.delenv("LES_LIGHT_QDRANT_URL", raising=False)
    with pytest.raises(RuntimeError, match="Перезапустите"):
        qdrant_client_options("http://127.0.0.1:6333")
