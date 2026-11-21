"""Private debug parser assets must not be publishable as source-bound release assets."""
import hashlib
import json
import pytest
from tools import native_asset


def test_api_tracks_transitive_embedded_files_and_ignores_unrelated_config(tmp_path):
    for crate in ('core', 'api', 'vector'):
        folder = tmp_path / crate
        (folder/'src').mkdir(parents=True)
        for name in ('Cargo.toml', 'Cargo.lock'):
            (folder/name).touch()
    config = tmp_path/'config'
    config.mkdir()
    (config/'version.json').write_text('{}')
    names = ['chat-commands', 'dataset-reader-schema', 'document-review-tool', 'table-document-tool']
    for name in names:
        (config/f'{name}.json').write_text('{}')
    (tmp_path/'core/src/lib.rs').write_text('\n'.join(
        f'const X{i}: &str = include_str!(\n  "../../config/{name}.json"\n);'
        for i, name in enumerate(names)))
    original = native_asset.source_digest('api', tmp_path)
    for name in names:
        path = config/f'{name}.json'
        path.write_text('{ }')
        assert native_asset.source_digest('api', tmp_path) != original, name
        path.write_text('{}')
    (config/'unused.json').write_text('[]')
    assert native_asset.source_digest('api', tmp_path) == original


def test_standard_package_rejects_edge_receipt(tmp_path):
    source = tmp_path/'les-api.exe'
    source.write_bytes(b'edge executable')
    receipt = dict(schema='les.native-api-asset.v1', file=source.name, remapped_paths=True,
                   source_sha256='current', sha256=hashlib.sha256(source.read_bytes()).hexdigest(), features=['edge'])
    (tmp_path/'api-receipt.json').write_text(json.dumps(receipt))
    with pytest.raises(ValueError, match='receipt'):
        native_asset.stage_asset('api',tmp_path/'standard',source,'current')


@pytest.mark.parametrize('change',[{'remapped_paths':False},{'revision':'wrong'},{'source_sha256':'old'},{'sha256':'corrupt'}])
def test_release_parser_asset_rejects_debug_wrong_revision_and_stale_source(tmp_path,change):
    source=tmp_path/'source/les-document-parsers.exe';source.parent.mkdir();source.write_bytes(b'controlled executable fixture')
    receipt={'schema':'les.document-parsers-asset.v1','file':source.name,'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
             'revision':'les.parsers.v1','source_sha256':'expected','remapped_paths':True,**change}
    (source.parent/'parser-receipt.json').write_text(json.dumps(receipt),encoding='utf8')
    with pytest.raises(ValueError,match='receipt/source'):
        native_asset.stage_asset('parsers',tmp_path/'package/native/parsers',source,'expected')
    assert not (tmp_path/'package').exists()
