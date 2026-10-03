from pathlib import Path
import pytest

from starlette.requests import Request

from proxy.routers.mail import _collector_environment, _outlook_collector_path


def test_light_uses_bundled_collector_after_install_and_keeps_state_separate(monkeypatch, tmp_path):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    monkeypatch.setenv("LES_RUNTIME_HOME", str(tmp_path / "application/runtime"))
    monkeypatch.setenv("LES_WINDOWS_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.delenv("LES_OUTLOOK_COLLECTOR_EXE", raising=False)
    bundled = tmp_path / "application/native/mail/LesLightMailPoller.exe"
    bundled.parent.mkdir(parents=True)
    bundled.write_bytes(b"collector")
    assert _outlook_collector_path() == bundled
    monkeypatch.setenv("LES_OUTLOOK_COLLECTOR_EXE", str(tmp_path / "explicit.exe"))
    assert _outlook_collector_path() == tmp_path / "explicit.exe"


def test_light_collector_has_own_binary_and_state(monkeypatch, tmp_path):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    monkeypatch.delenv("LES_WINDOWS_STATE_ROOT", raising=False)
    monkeypatch.delenv("LES_OUTLOOK_COLLECTOR_EXE", raising=False)
    assert _outlook_collector_path() == tmp_path / "LES Light/bin/LesLightMailPoller.exe"


def test_light_mail_follows_actual_api_port(monkeypatch, tmp_path):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    monkeypatch.setenv("LES_WINDOWS_STATE_ROOT", str(tmp_path))
    request = Request({"type": "http", "scheme": "http", "server": ("127.0.0.1", 57123), "path": "/api/mail/collector/run", "headers": []})
    environment = _collector_environment(request)
    assert Path(environment["LES_MAIL_STATE_ROOT"]) == tmp_path / "mail"
    assert (tmp_path / "mail/collector_url.txt").read_text() == "http://127.0.0.1:57123/api/mail/collector/import"
    assert not list((tmp_path / "mail").glob("*.tmp"))


def test_full_collector_cannot_be_selected_in_light(monkeypatch, tmp_path):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "full")
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    monkeypatch.delenv("LES_WINDOWS_STATE_ROOT", raising=False)
    monkeypatch.delenv("LES_OUTLOOK_COLLECTOR_EXE", raising=False)
    with pytest.raises(ValueError, match="LES Light only"):
        _outlook_collector_path()
