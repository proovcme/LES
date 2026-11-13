"""Private debug parser assets must not be publishable as source-bound release assets."""
import hashlib
import json
import pytest
from tools import native_asset


@pytest.mark.parametrize('change',[{'remapped_paths':False},{'revision':'wrong'},{'source_sha256':'old'},{'sha256':'corrupt'}])
def test_release_parser_asset_rejects_debug_wrong_revision_and_stale_source(tmp_path,change):
    source=tmp_path/'source/les-document-parsers.exe';source.parent.mkdir();source.write_bytes(b'controlled executable fixture')
    receipt={'schema':'les.document-parsers-asset.v1','file':source.name,'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
             'revision':'les.parsers.v1','source_sha256':'expected','remapped_paths':True,**change}
    (source.parent/'parser-receipt.json').write_text(json.dumps(receipt),encoding='utf8')
    with pytest.raises(ValueError,match='receipt/source'):
        native_asset.stage_asset('parsers',tmp_path/'package/native/parsers',source,'expected')
    assert not (tmp_path/'package').exists()
