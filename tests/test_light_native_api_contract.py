import json
from tools import native_api_contract as contract


def test_light_native_api_baseline_preserves_surface_and_shapes():
    current=contract.isolated_capture()
    baseline=json.loads(contract.OUTPUT.read_text(encoding='utf-8'))
    assert current==baseline
    pairs={(row['method'],row['path']) for row in baseline['routes']}
    for pair in [('POST','/api/chat'),('GET','/api/rag/datasets'),('GET','/api/projects')]:
        assert pair in pairs
    assert not any(path.startswith(('/api/smeta','/api/rim')) for _,path in pairs)
