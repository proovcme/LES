import hashlib
import io
import tarfile
from pathlib import Path
from types import SimpleNamespace

import pytest

from tools import light_launcher, light_runtime_assets


def test_macos_state_and_home_are_isolated(tmp_path, monkeypatch):
    monkeypatch.setattr(light_launcher.sys, 'platform', 'darwin')
    monkeypatch.setenv('HOME', str(tmp_path))
    monkeypatch.setenv('OPENAI_API_KEY', 'foreign-secret')
    monkeypatch.setenv('LES_STATE_ROOT', 'foreign-state')
    state = light_launcher.default_state()
    assert state == tmp_path / 'Library/Application Support/LES Light'
    env = light_launcher.child_environment(tmp_path, state, SimpleNamespace(url='http://127.0.0.1:1', api_key='own'), 2, 3, 'test')
    assert env['HOME'] == str(tmp_path)
    assert env['LES_STATE_ROOT'] == str(state)
    assert 'foreign-secret' not in env.values()
    assert 'foreign-state' not in env.values()


def test_native_tar_extracts_only_verified_binary(tmp_path):
    archive = tmp_path / 'q.tar.gz'
    with tarfile.open(archive, 'w:gz') as out:
        for name, data in [('../../qdrant', b'binary'), ('../unrelated', b'ignored')]:
            item = tarfile.TarInfo(name); item.size = len(data)
            out.addfile(item, io.BytesIO(data))
    dest = tmp_path / 'native'
    with pytest.raises(ValueError, match='Контрольная сумма'):
        light_runtime_assets.verified_executable(archive, '0'*64, dest, executable_name='qdrant')
    assert not dest.exists()
    result = light_runtime_assets.verified_executable(archive, hashlib.sha256(archive.read_bytes()).hexdigest(), dest, executable_name='qdrant')
    assert result == dest / 'qdrant'
    assert result.read_bytes() == b'binary'
    assert result.stat().st_mode & 0o111
    assert not (tmp_path / 'unrelated').exists()


def test_tar_symlink_is_not_installed(tmp_path):
    archive = tmp_path / 'q.tar.gz'
    with tarfile.open(archive, 'w:gz') as out:
        item = tarfile.TarInfo('qdrant'); item.type = tarfile.SYMTYPE; item.linkname = '/bin/sh'
        out.addfile(item)
    with pytest.raises(ValueError):
        light_runtime_assets.verified_executable(archive, hashlib.sha256(archive.read_bytes()).hexdigest(), tmp_path/'native', executable_name='qdrant')
    assert not (tmp_path/'native').exists()


@pytest.mark.parametrize('machine,target', [('arm64','aarch64-apple-darwin'), ('x86_64','x86_64-apple-darwin')])
def test_macos_target(machine, target, monkeypatch):
    monkeypatch.setattr(light_runtime_assets.sys,'platform','darwin')
    monkeypatch.setattr(light_runtime_assets.platform,'machine',lambda:machine)
    assert light_runtime_assets.host_target() == target


def test_macos_ocr_is_selected_without_hidden_model(monkeypatch):
    from backend import ocr_parser
    from backend.macos_ocr import MacOSOCRParser
    monkeypatch.setattr(ocr_parser.sys, 'platform', 'darwin')
    monkeypatch.delenv('RAG_OCR_BACKEND', raising=False)
    assert isinstance(ocr_parser.make_ocr_parser(), MacOSOCRParser)


def test_reopen_refuses_foreign_browser_url(tmp_path,monkeypatch):
    import json
    (tmp_path/'launcher-status.json').write_text(json.dumps({'phase':'ready','instance_id':'own',
        'api_url':'http://127.0.0.1:1234','ui_url':'https://example.com/classic'}))
    monkeypatch.setattr(light_launcher.webbrowser,'open',lambda url:pytest.fail('must not open foreign URL'))
    assert not light_launcher.reopen_owned_browser(tmp_path)
