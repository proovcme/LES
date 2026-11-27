import hashlib
import json
from pathlib import Path

import pytest

from tools import build_light_frontend as builder


@pytest.fixture
def tree(tmp_path):
    root = tmp_path / 'source'
    frontend = root / builder.FRONTEND
    (frontend / 'src').mkdir(parents=True)
    for name in builder.INPUTS:
        (frontend / name).write_text('{}', encoding='utf-8')
    (frontend / 'src/main.ts').write_text('export const title="Лес"', encoding='utf-8')
    shared_nav = root / 'qdrant_visualizer/app-nav.css'
    shared_nav.parent.mkdir()
    shared_nav.write_text('.app-nav { display: flex; }', encoding='utf-8')
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
    checked = builder.validated_assets(root)
    assert checked == (dist, {'index.html', 'assets/main.js', 'build-receipt.json'})
    assert not runtime.exists()  # Readiness checking must not stage or rebuild.
    builder.stage(root, runtime)
    assert (runtime / builder.FRONTEND / 'dist/index.html').is_file()
    assert not (runtime / builder.FRONTEND / 'node_modules').exists()
    assert not (runtime / builder.FRONTEND / 'src').exists()


@pytest.mark.parametrize('kind', ['changed-source', 'changed-bundle', 'extra-file', 'traversal', 'missing-receipt'])
def test_stale_or_unlisted_assets_never_enter_the_package(tree, kind):
    root, frontend, dist, runtime = tree
    if kind == 'changed-source':
        (frontend / 'src/main.ts').write_text('new source')
    elif kind == 'changed-bundle':
        (dist / 'assets/main.js').write_text('modified bundle')
    elif kind == 'extra-file':
        (dist / 'private.txt').write_text('private')
    elif kind == 'missing-receipt':
        (dist / 'build-receipt.json').unlink()
    else:
        receipt = json.loads((dist / 'build-receipt.json').read_text())
        receipt['files']['../secret.txt'] = hashlib.sha256(b'secret').hexdigest()
        (dist / 'build-receipt.json').write_text(json.dumps(receipt))
    with pytest.raises((ValueError, RuntimeError)):
        builder.stage(root, runtime)
    assert not runtime.exists()
