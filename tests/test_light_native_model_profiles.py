"""Frozen pre-migration cases ensure context/tool limits do not change with Rust ownership."""
import json
from pathlib import Path
from backend import native_core


def test_capacity_and_restrictions_match_250_frozen_cases():
    cases=json.loads((Path(__file__).parent/'fixtures/native_model_profiles.json').read_text(encoding='utf-8'))
    assert len(cases)==250
    for index,case in enumerate(cases):
        result=native_core.required('model_profile',request={
            'schema':'les.model-profile.v1','capacity':case['capacity'],
            'operator':case['operator'],'restrictions':case['restrictions'],
        })
        assert result['preset']==case['expected'],index


def test_python_contract_is_only_a_typed_facade():
    from dataclasses import asdict
    from proxy.services.model_execution_preset_service import BackendCapacity,resolve_execution_preset
    capacity=BackendCapacity('model_connection','Qwen3.5-9B',24576,True,'probe')
    actual=resolve_execution_preset(capacity,operator={'input_tokens':24000},restrictions={'max_tools':2})
    result=native_core.required('model_profile',request={
        'schema':'les.model-profile.v1','capacity':asdict(capacity),
        'operator':{'input_tokens':24000},'restrictions':{'max_tools':2},
    })
    assert json.loads(json.dumps(asdict(actual)))==result['preset']
