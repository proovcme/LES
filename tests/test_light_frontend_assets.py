import hashlib
import json
from pathlib import Path

from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
import pytest

from proxy import light_frontend
from tools import build_light_frontend as builder


def test_static_ui_is_local_and_has_no_inline_script_permissions(tmp_path, monkeypatch):
    (tmp_path / 'index.html').write_text('<html lang="ru">Лес</html>', encoding='utf-8')
    (tmp_path / 'assets').mkdir()
    (tmp_path / 'assets/main.js').write_text('export const name="лес"', encoding='utf-8')
    monkeypatch.setattr(light_frontend, 'FRONTEND_ROOT', tmp_path)
    monkeypatch.setenv('LES_LIGHT_LEGACY_UI_URL', 'http://127.0.0.1:55444/classic')
    app = FastAPI()
    light_frontend.mount_frontend(app)
    app.dependency_overrides[light_frontend.require_user] = lambda: object()
    with TestClient(app) as client:
        response = client.get('/app/')
        assert response.status_code == 200
        assert 'Лес' in response.text
        csp = response.headers['content-security-policy']
        assert "script-src 'self'" in csp and "connect-src 'self'" in csp
        assert 'unsafe-inline' not in csp and 'unsafe-eval' not in csp
        assert response.headers['cache-control'] == 'no-store'
        assert client.get('/app/assets/main.js').status_code == 200
        assert client.get('/app/%2e%2e/private').status_code == 404
        assert client.get('/api/light/frontend').json()['legacy_url'] == 'http://127.0.0.1:55444/classic'
        monkeypatch.setenv('LES_LIGHT_LEGACY_UI_URL', 'https://example.org/classic')
        assert client.get('/api/light/frontend').json()['legacy_url'] is None
        def deny():
            raise HTTPException(401, 'Unauthorized')
        app.dependency_overrides[light_frontend.require_user] = deny
        assert client.get('/api/light/frontend').status_code == 401


@pytest.fixture
def tree(tmp_path):
    root = tmp_path / 'source'
    frontend = root / builder.FRONTEND
    (frontend / 'src').mkdir(parents=True)
    for name in builder.INPUTS:
        (frontend / name).write_text('{}', encoding='utf-8')
    (frontend / 'src/main.ts').write_text('export const title="Лес"', encoding='utf-8')
    dist = frontend / 'dist'
    (dist / 'assets').mkdir(parents=True)
    (dist / 'index.html').write_text('<html>Лес</html>', encoding='utf-8')
    (dist / 'assets/main.js').write_text('export const title="Лес"', encoding='utf-8')
    receipt = dict(schema='les.frontend.v1', source_sha256=builder.source_digest(root),
                   files={name: builder.file_digest(dist / name) for name in ['index.html', 'assets/main.js']})
    (dist / 'build-receipt.json').write_text(json.dumps(receipt), encoding='utf-8')
    return root, frontend, dist, tmp_path / 'runtime'


def test_compiled_assets_stage_without_node_dependencies(tree):
    root, frontend, dist, runtime = tree
    (frontend / 'node_modules').mkdir()
    (frontend / 'node_modules/private.txt').write_text('not a runtime asset')
    builder.stage(root, runtime)
    assert (runtime / builder.FRONTEND / 'dist/index.html').is_file()
    assert not (runtime / builder.FRONTEND / 'node_modules').exists()
    assert not (runtime / builder.FRONTEND / 'src').exists()


@pytest.mark.parametrize('kind', ['changed-source', 'changed-bundle', 'extra-file', 'traversal'])
def test_stale_or_unlisted_assets_never_enter_the_package(tree, kind):
    root, frontend, dist, runtime = tree
    if kind == 'changed-source':
        (frontend / 'src/main.ts').write_text('new source')
    elif kind == 'changed-bundle':
        (dist / 'assets/main.js').write_text('modified bundle')
    elif kind == 'extra-file':
        (dist / 'private.txt').write_text('private')
    else:
        receipt = json.loads((dist / 'build-receipt.json').read_text())
        receipt['files']['../secret.txt'] = hashlib.sha256(b'secret').hexdigest()
        (dist / 'build-receipt.json').write_text(json.dumps(receipt))
    with pytest.raises((ValueError, RuntimeError)):
        builder.stage(root, runtime)
    assert not runtime.exists()


def test_launcher_svelte_mode_preserves_owned_legacy_services(tmp_path, monkeypatch):
    from types import SimpleNamespace
    from tools import light_launcher
    index = tmp_path / 'desktop/light/frontend/dist/index.html'
    index.parent.mkdir(parents=True)
    index.write_text('<html/>')
    stack = light_launcher.LightStack(tmp_path, tmp_path, tmp_path / 'qdrant.exe', frontend='svelte')
    monkeypatch.setattr(stack.qdrant, 'start', lambda: None)
    stack.qdrant.url = 'http://127.0.0.1:55440'
    monkeypatch.setattr(light_launcher, 'free_port', iter([55441, 55442]).__next__)
    environments = []
    monkeypatch.setattr(stack, 'spawn', lambda _command, env, _name: environments.append(env) or SimpleNamespace(poll=lambda: None))
    monkeypatch.setattr(stack, 'wait_ready', lambda *_args: None)
    assert stack.start(None) == 'http://127.0.0.1:55441/app/'
    assert len(environments) == 2
    assert environments[0]['LES_LIGHT_LEGACY_UI_URL'] == 'http://127.0.0.1:55442/classic'


def test_missing_ui_fails_before_any_service_is_started(tmp_path, monkeypatch):
    from tools import light_launcher
    stack = light_launcher.LightStack(tmp_path, tmp_path, tmp_path / 'qdrant.exe', frontend='svelte')
    monkeypatch.setattr(stack.qdrant, 'start', lambda: pytest.fail('must not start'))
    with pytest.raises(RuntimeError, match='интерфейса'):
        stack.start(None)
