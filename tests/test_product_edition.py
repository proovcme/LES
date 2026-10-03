import pytest

from backend.product_edition import is_light, profile_modes


def test_full_edition_cannot_be_started_from_light_repository(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "full")
    with pytest.raises(ValueError, match="LES Light only"):
        is_light()


def test_light_excludes_domain_modes_and_workbook_execution(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    from proxy.services.chat_profile_service import canonical_profile_mode
    from proxy.services.tool_harness_service import ToolHarness
    assert profile_modes() == ("search", "agent")
    for mode in ("estimator", "smeta", "smeta_harness", "engineer"):
        with pytest.raises(ValueError, match="LES RAG"):
            canonical_profile_mode(mode)
    harness = ToolHarness()
    names = {row["name"] for row in harness.registry()["tools"]}
    assert {"search_sources", "read_source", "web_search", "web_read"} <= names
    assert "build_lsr_workbook" not in names
    assert "build_lsr_workbook" not in harness.directly_executable_tool_names()


def test_unknown_edition_is_not_silently_full(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "ligth")
    with pytest.raises(ValueError):
        is_light()
