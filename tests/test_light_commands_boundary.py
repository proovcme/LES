from proxy.services.command_service import handle_command, list_commands


def test_light_command_palette_and_handler_exclude_estimates(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    titles = [entry["cmd"] for entry in list_commands()]
    assert "/смета" not in titles
    assert "/команды" in titles
    result = handle_command("/смета")
    assert result["command"]["action"] == "unknown"
    assert "смет" not in handle_command("/команды")["answer"].lower()
