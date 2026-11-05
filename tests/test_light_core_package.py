import hashlib
import json
import pytest
from tools.build_native_core import stage_asset


def test_core_asset_requires_receipt_and_refuses_overwriting_rollback(tmp_path):
    source=tmp_path/'built/les_core.dll'; source.parent.mkdir(); source.write_bytes(b'fixture asset')
    receipt={'schema':'les.core-asset.v1','abi':1,'file':source.name,
             'remapped_paths':True,
             'sha256':hashlib.sha256(source.read_bytes()).hexdigest()}
    (source.parent/'core-receipt.json').write_text(json.dumps(receipt))
    target=stage_asset(tmp_path/'payload/native/core',source)
    assert target.read_bytes()==source.read_bytes()
    with pytest.raises(FileExistsError):stage_asset(target.parent,source)
    source.write_bytes(b'changed asset')
    with pytest.raises(ValueError,match='receipt'):stage_asset(tmp_path/'bad',source)
    assert not (tmp_path/'bad').exists()
