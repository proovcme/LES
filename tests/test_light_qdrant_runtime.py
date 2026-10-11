from pathlib import Path
import json
import os

import pytest

from backend.light_qdrant_runtime import LightQdrantRuntime, PortCollisionError


def test_missing_binary_does_not_create_state(tmp_path):
    runtime = LightQdrantRuntime(tmp_path / "missing.exe", tmp_path / "state")
    with pytest.raises(FileNotFoundError, match="Qdrant"):
        runtime.start()
    assert not runtime.root.exists()


def test_storage_lock_prevents_second_owner_and_is_released(tmp_path):
    first = LightQdrantRuntime(Path("qdrant.exe"), tmp_path)
    second = LightQdrantRuntime(Path("qdrant.exe"), tmp_path)
    first._acquire()
    try:
        with pytest.raises(RuntimeError, match="LES RAG"):
            second._acquire()
        second.stop()
        # Stopping a rejected owner must not unlock the real owner.
        with pytest.raises(RuntimeError):
            second._acquire()
    finally:
        first.stop()
    second._acquire()
    second.stop()


def test_spawn_failure_releases_lock_and_removes_credentials(tmp_path, monkeypatch):
    executable = tmp_path / "qdrant.exe"
    executable.touch()
    runtime = LightQdrantRuntime(executable, tmp_path)
    monkeypatch.setenv("QDRANT__STORAGE__STORAGE_PATH", "foreign-state")

    def fail(*args, **kwargs):
        assert "QDRANT__STORAGE__STORAGE_PATH" not in kwargs["env"]
        config = json.loads(runtime._config.read_text(encoding="utf-8"))
        configured = Path(config["storage"]["storage_path"])
        configured.mkdir()
        assert configured.samefile(runtime.root / "storage")
        if os.name == "nt":
            assert config["storage"]["storage_path"].startswith("\\\\?\\")
            assert config["storage"]["snapshots_path"].startswith("\\\\?\\")
        raise OSError("spawn failed")

    monkeypatch.setattr("backend.light_qdrant_runtime.subprocess.Popen", fail)
    with pytest.raises(RuntimeError, match="Не удалось запустить Qdrant") as failure:
        runtime.start()
    assert isinstance(failure.value.__cause__, OSError)
    assert not (runtime.root / "runtime.yaml").exists()
    assert runtime.api_key == ""
    assert runtime._lock is None
    runtime._acquire()
    runtime.stop()


def test_port_collision_retries_are_bounded(tmp_path, monkeypatch):
    runtime = LightQdrantRuntime(tmp_path / "qdrant.exe", tmp_path)
    attempts = []

    def collide():
        attempts.append(1)
        raise PortCollisionError("occupied")

    monkeypatch.setattr(runtime, "_start_once", collide)
    with pytest.raises(PortCollisionError, match="трёх"):
        runtime.start()
    assert len(attempts) == 3


@pytest.mark.parametrize("denied_file", ["runtime.lock", "runtime.yaml"])
def test_storage_access_denial_is_actionable_and_never_spawns(tmp_path, monkeypatch, denied_file):
    executable = tmp_path / "qdrant.exe"
    executable.touch()
    runtime = LightQdrantRuntime(executable, tmp_path / "state")
    original_open = Path.open

    def denied_open(path, *args, **kwargs):
        if path.name == denied_file:
            raise PermissionError("test access denied")
        return original_open(path, *args, **kwargs)

    def unexpected_spawn(*args, **kwargs):
        pytest.fail("must not spawn when storage cannot be written")

    monkeypatch.setattr(Path, "open", denied_open)
    monkeypatch.setattr("backend.light_qdrant_runtime.subprocess.Popen", unexpected_spawn)
    with pytest.raises(RuntimeError, match="Нет доступа"):
        runtime.start()
    assert runtime._lock is None
    assert runtime.api_key == ""
    assert not (runtime.root / "runtime.yaml").exists()
