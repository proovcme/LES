import json

import pytest

from tools import native_api_contract as contract


def test_public_api_matches_the_native_snapshot_without_python_upstream():
    program = contract.executable()
    if not program.is_file():
        pytest.skip("Build les-api to verify the public API")
    baseline = json.loads(contract.OUTPUT.read_text(encoding="utf-8"))
    assert contract.capture(program) == baseline
