from types import SimpleNamespace

import pytest
from pydantic import ValidationError

from proxy.routers import memory


def test_light_memory_contract_has_no_estimate_controls(monkeypatch):
    store = SimpleNamespace(status=lambda: {
        "entries": {"fact": 2}, "queue": {}, "smeta_traces": 4, "open_conflicts": 0,
    })
    monkeypatch.setattr(memory, "_store", lambda: store)
    monkeypatch.setattr(memory, "load_memory_config", lambda _store: SimpleNamespace(
        mode=SimpleNamespace(value="on"),
    ))
    status = memory.get_light_memory_status()
    assert status["entries"] == {"fact": 2}
    assert "smeta_traces" not in status
    assert not any("smeta" in name for name in memory.LightMemoryConfigUpdate.model_fields)
    with pytest.raises(ValidationError):
        memory.LightMemoryConfigUpdate.model_validate({"mode": "on", "smeta_recall": "route_reuse"})


def test_light_memory_save_forces_estimate_features_off(monkeypatch):
    saved = {}
    monkeypatch.setattr(memory, "_store", lambda: object())
    monkeypatch.setattr(memory, "update_memory_config", lambda _store, **kwargs: saved.update(kwargs))
    response = memory.put_light_memory_config(memory.LightMemoryConfigUpdate(mode="on"))
    assert response["mode"] == "on"
    assert saved == {"mode": "on", "smeta_capture": False, "smeta_recall": "off"}
