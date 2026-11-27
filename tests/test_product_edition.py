import pytest

from backend.product_edition import is_light


def test_full_edition_cannot_be_started_from_light_repository(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "full")
    with pytest.raises(ValueError, match="LES Light only"):
        is_light()


def test_unknown_edition_is_not_silently_full(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "ligth")
    with pytest.raises(ValueError):
        is_light()
