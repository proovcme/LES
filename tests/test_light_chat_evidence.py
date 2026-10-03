"""The generic Light chat keeps evidence and web tools without estimate tools."""

from proxy.services import chat_evidence_application_service as service
from proxy.services.web_research_config_service import WebResearchConfig


def test_light_web_tools_remain_available(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    tools = ["web_search", "web_read", "filesystem_search"]
    visible = service.web_tools_for_request(tools, WebResearchConfig("simple", "", "", ""))
    assert "web_search" in visible
    assert "web_read" in visible
    assert "filesystem_search" in visible


def test_light_does_not_activate_estimate_model_rag(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    assert not hasattr(service, 'profile_uses_model_driven_retrieval')
    assert not hasattr(service, 'parse_model_rag_result')
    assert not hasattr(service, 'retrieve_smeta_norm_cards')


def test_light_chat_router_has_no_workbook_executor(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    from proxy.routers import chat

    assert not hasattr(chat, "_execute_chat_workbook_tool")
    assert not hasattr(chat, "_format_harness")
