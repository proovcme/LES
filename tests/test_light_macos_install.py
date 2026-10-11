"""Bundle publication tests; native tools are stubbed, no downloads or installs."""
import json
from pathlib import Path
import subprocess

import pytest

from backend.light_processes import InstanceLock
from tools import install_light_macos as installer


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
    monkeypatch.setattr(installer, 'stage_code', stage_code)
    monkeypatch.setattr(installer, 'stage', stage)
    monkeypatch.setattr(installer, 'host_target', lambda: 'aarch64-apple-darwin')
    monkeypatch.setattr(installer.subprocess, 'run', lambda command, **kwargs: calls.append(command))
    return calls


@pytest.mark.parametrize('failure', ['stage', 'download', 'compile', 'dependencies', 'interrupt'])
def test_failed_install_can_retry_same_path(tmp_path, monkeypatch, build_environment, failure):
    app = tmp_path / 'Приложения с пробелами/LES.app'
    state = tmp_path / 'state'
    state.mkdir()
    (state / 'keep.txt').write_text('user state')
    with monkeypatch.context() as fault:
        def fail(*args, **kwargs):
            raise OSError('simulated build failure')
        if failure in {'stage', 'download'}:
            fault.setattr(installer, 'stage_code' if failure == 'stage' else 'stage', fail)
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
