import asyncio

import pytest
from fastapi import HTTPException

from proxy.routers import settings


def test_light_settings_skip_legacy_mlx_probe_and_smeta_fields(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    monkeypatch.setenv("LES_SMETA_GOOGLE_MODEL", "internal-model")

    class ForbiddenClient:
        def __init__(self, *args, **kwargs):
            raise AssertionError("Light must not contact the legacy MLX host")

    monkeypatch.setattr(settings.httpx, "AsyncClient", ForbiddenClient)
    result = asyncio.run(settings.get_settings(_user=object()))
    assert "smeta_google_model" not in result
    assert "smeta_document_model" not in result
    assert "google_api_key_set" not in result


def test_light_rejects_legacy_smeta_settings_before_file_write(monkeypatch, tmp_path):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    target = tmp_path / "settings.env"
    monkeypatch.setattr(settings, "ENV_PATH", target)
    with pytest.raises(HTTPException, match="Сметные настройки"):
        asyncio.run(settings.save_settings(settings.SettingsRequest(smeta_google_model="anything"), _admin=object()))
    assert not target.exists()
