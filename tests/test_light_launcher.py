import os
from pathlib import Path
import subprocess
import sys
import time
from types import SimpleNamespace

import psutil
import pytest

from backend.light_processes import InstanceLock
from tools.light_launcher import child_environment


def test_both_services_spawn_before_waiting_and_collision_restarts_pair(tmp_path, monkeypatch):
    from tools import light_launcher as module
    stack = module.LightStack(tmp_path, tmp_path, tmp_path / "qdrant.exe", read_only=True)
    events = []
    stack.qdrant = SimpleNamespace(start=lambda: None, url="http://localhost:1", api_key="test")
    ports = iter([10001, 10002, 10003, 10004])
    monkeypatch.setattr(module, "free_port", lambda: next(ports))
    monkeypatch.setattr(module, "port_is_free", lambda _: False)
    def spawn(command, env, name):
        events.append((name, env["PROXY_URL"]))
        assert env["LES_STARTUP_BACKGROUND_MUTATIONS"] == "false"
        return SimpleNamespace(poll=lambda: 1 if len(events) == 2 else None)
    def ready(*args):
        assert len(events) in (2, 4)
        if len(events) == 2:
            raise RuntimeError("bind collision")
    monkeypatch.setattr(stack, "spawn", spawn)
    monkeypatch.setattr(stack, "wait_ready", ready)
    monkeypatch.setattr(stack, "stop", lambda: None)
    assert stack.start(None) == "http://127.0.0.1:10004/classic"
    assert events == [("api-console.txt", "http://127.0.0.1:10001"),
                      ("ui-console.txt", "http://127.0.0.1:10001"),
                      ("api-console.txt", "http://127.0.0.1:10003"),
                      ("ui-console.txt", "http://127.0.0.1:10003")]


def test_state_journey_rejects_python_importing_another_runtime(tmp_path):
    from tools.light_state_journey import run
    output = tmp_path / "evidence"
    with pytest.raises(ValueError, match="Runtime mismatch"):
        run(tmp_path / "different-runtime", tmp_path / "qdrant.exe", output)
    assert not output.exists()


def test_launcher_lock_cannot_be_released_by_rejected_owner(tmp_path):
    first, second = InstanceLock(tmp_path / "owner.lock"), InstanceLock(tmp_path / "owner.lock")
    assert first.acquire()
    try:
        assert not second.acquire()
        second.release()
        assert not second.acquire()
    finally:
        first.release()
    assert second.acquire()
    second.release()


def test_launcher_ignores_full_les_paths_models_and_credentials(tmp_path, monkeypatch):
    for key in ("LES_WINDOWS_STATE_ROOT", "RAG_META_DB_PATH", "QDRANT_URL", "OPENAI_API_KEY", "LLM_MODEL", "OLLAMA_API_KEY", "PYTHONHOME"):
        monkeypatch.setenv(key, "foreign-value")
    state = tmp_path / "state"
    env = child_environment(tmp_path, state, SimpleNamespace(url="http://127.0.0.1:1234", api_key="own"), 1235, 1236, "instance")
    assert "foreign-value" not in env.values()
    assert env["LES_WINDOWS_STATE_ROOT"] == str(state)
    assert env["LES_PRODUCT_EDITION"] == "light"
    assert env["LES_LIGHT_QDRANT_API_KEY"] == "own"


@pytest.mark.skipif(os.name != "nt", reason="Windows job object")
def test_abrupt_launcher_death_terminates_owned_child():
    code = "from backend.light_processes import attach_lifetime_job; import subprocess,sys,time; job=attach_lifetime_job(); child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); print(child.pid,flush=True); time.sleep(60)"
    parent = subprocess.Popen([sys.executable, "-c", code], cwd=Path(__file__).resolve().parents[1], stdout=subprocess.PIPE, text=True)
    child_pid = None
    try:
        child_pid = int(parent.stdout.readline().strip())
        assert psutil.pid_exists(child_pid)
        parent.kill()
        parent.wait(timeout=5)
        deadline = time.monotonic() + 5
        while psutil.pid_exists(child_pid) and time.monotonic() < deadline:
            time.sleep(0.05)
        assert not psutil.pid_exists(child_pid)
    finally:
        if parent.poll() is None:
            parent.kill()
            parent.wait(timeout=5)
        if child_pid and psutil.pid_exists(child_pid):
            psutil.Process(child_pid).kill()
