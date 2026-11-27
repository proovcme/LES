"""The Windows tool registry excludes inherited estimate write operations."""
from tests.test_light_native_chat_profiles import profiles


def test_light_tool_registry_does_not_offer_workbook_builders(profiles):
    client, _, _ = profiles
    names = {tool['name'] for tool in client.get('/api/tools/registry').raise_for_status().json()['tools']}
    assert {'build_lsr_workbook', 'build_vor_workbook'}.isdisjoint(names)
