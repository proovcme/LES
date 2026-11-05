"""Bundle publication tests; native tools are stubbed, no downloads or installs."""
import json
from pathlib import Path
import subprocess

import pytest

from backend.light_processes import InstanceLock
from tools import install_light_macos as installer


def test_publish_retries_transient_handle_and_preserves_bundle(tmp_path, monkeypatch):
    candidate = tmp_path/'staging/LES.app'; candidate.mkdir(parents=True)
    (candidate/'keep').write_text('complete bundle')
    target = tmp_path/'LES.app'; calls = []; sleeps = []
    original = Path.rename
    def rename(path, destination):
        calls.append(path)
        if len(calls) < 3:
            error = PermissionError('scanner handle'); error.winerror = 5; raise error
        return original(path, destination)
    monkeypatch.setattr(Path, 'rename', rename)
    monkeypatch.setattr(installer.time, 'sleep', sleeps.append)
    installer._publish_app(candidate, target)
    assert len(calls) == 3 and len(sleeps) == 2
    assert (target/'keep').read_text() == 'complete bundle' and not candidate.exists()


def test_publish_retry_is_bounded_and_does_not_remove_source(tmp_path, monkeypatch):
    candidate = tmp_path/'candidate'; candidate.mkdir(); target = tmp_path/'LES.app'; calls = []
    def rename(path, destination):
        calls.append(path); error = PermissionError('locked'); error.winerror = 32; raise error
    monkeypatch.setattr(Path, 'rename', rename)
    monkeypatch.setattr(installer.time, 'sleep', lambda _: None)
    with pytest.raises(PermissionError): installer._publish_app(candidate, target)
    assert len(calls) == 8 and candidate.exists() and not target.exists()


def test_publish_retry_preserves_application_created_during_wait(tmp_path, monkeypatch):
    candidate = tmp_path/'candidate'; candidate.mkdir(); target = tmp_path/'LES.app'
    def rename(path, destination):
        target.mkdir(); (target/'user-file').write_text('preserve')
        error = PermissionError('locked'); error.winerror = 5; raise error
    monkeypatch.setattr(Path, 'rename', rename)
    monkeypatch.setattr(installer.time, 'sleep', lambda _: None)
    with pytest.raises(FileExistsError): installer._publish_app(candidate, target)
    assert candidate.exists() and (target/'user-file').read_text() == 'preserve'


@pytest.fixture
def build_environment(tmp_path, monkeypatch):
    monkeypatch.setattr(installer.sys, 'platform', 'darwin')
    monkeypatch.setattr(installer.shutil, 'which', lambda _: 'uv')
    # Ensure developer-host cached assets cannot affect this test.
    monkeypatch.setattr(installer, '__file__', str(tmp_path / 'repo/tools/install_light_macos.py'))
    calls = []
    def stage_code(resources):
        runtime = resources / 'runtime'
        (runtime / 'config').mkdir(parents=True)
        (runtime / 'config/version.json').write_text(json.dumps({'product_version': '0.1.0', 'build_number': 763}))
    def stage(destination, **kwargs):
        destination.mkdir(parents=True)
        (destination / 'qdrant').write_bytes(b'fixture')
    def build_core(destination):
        destination.mkdir(parents=True)
        (destination / 'libles_core.dylib').write_bytes(b'fixture')
        (destination / 'core-receipt.json').write_text('{}')
    monkeypatch.setattr(installer, 'stage_code', stage_code)
    monkeypatch.setattr(installer, 'stage', stage)
    monkeypatch.setattr(installer, 'build_core', build_core)
    monkeypatch.setattr(installer, 'host_target', lambda: 'aarch64-apple-darwin')
    monkeypatch.setattr(installer.subprocess, 'run', lambda command, **kwargs: calls.append(command))
    return calls


@pytest.mark.parametrize('failure', ['stage', 'core', 'download', 'compile', 'dependencies', 'interrupt'])
def test_failed_install_can_retry_same_path(tmp_path, monkeypatch, build_environment, failure):
    app = tmp_path / 'Приложения с пробелами/LES.app'
    state = tmp_path / 'state'
    state.mkdir()
    (state / 'keep.txt').write_text('user state')
    with monkeypatch.context() as fault:
        def fail(*args, **kwargs):
            raise OSError('simulated build failure')
        if failure in {'stage', 'core', 'download'}:
            fault.setattr(installer, {'stage': 'stage_code', 'core': 'build_core', 'download': 'stage'}[failure], fail)
        else:
            def run(command, **kwargs):
                if failure == 'interrupt':
                    raise KeyboardInterrupt()
                if (failure == 'compile' and 'swiftc' in command) or (failure == 'dependencies' and 'sync' in command):
                    raise subprocess.CalledProcessError(1, command)
            fault.setattr(installer.subprocess, 'run', run)
        with pytest.raises((OSError, subprocess.CalledProcessError, KeyboardInterrupt)):
            installer.install(app, state)
    assert not app.exists()
    assert not list(app.parent.glob(f'.{app.name}.install-*'))
    assert (state / 'keep.txt').read_text() == 'user state'
    assert installer.install(app, state) == app.resolve()
    assert (app / 'Contents/MacOS/LES').is_file()
    assert (app / 'Contents/Info.plist').is_file()
    assert (app / 'Contents/Resources/native/core/libles_core.dylib').is_file()
    assert (app / 'Contents/Resources/native/core/core-receipt.json').is_file()
    assert not list(app.parent.glob(f'.{app.name}.install-*'))
    assert any('venv' in command and '--relocatable' in command for command in build_environment)
    assert any('sync' in command and '--no-editable' in command for command in build_environment)


def test_existing_app_and_concurrent_install_are_preserved(tmp_path, build_environment):
    app = tmp_path / 'LES.app'
    lock = InstanceLock(tmp_path / '.LES.app.install.lock')
    assert lock.acquire()
    try:
        with pytest.raises(RuntimeError, match='Another installation'):
            installer.install(app, tmp_path / 'state')
    finally:
        lock.release()
    app.mkdir()
    (app / 'keep.txt').write_text('existing app')
    with pytest.raises(FileExistsError):
        installer.install(app, tmp_path / 'state')
    assert (app / 'keep.txt').read_text() == 'existing app'
