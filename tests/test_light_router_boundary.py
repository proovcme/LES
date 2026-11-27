"""The explicit Light tool registry keeps estimate write operations out."""
from proxy.services.tool_registry_service import canonical_tool_registry


def test_light_tool_registry_does_not_offer_workbook_builders():
    names = {item.contract.name for item in canonical_tool_registry().registrations()}
    assert "build_lsr_workbook" not in names
    assert "build_vor_workbook" not in names
