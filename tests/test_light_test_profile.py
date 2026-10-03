"""The release gate follows the standalone Light product boundary."""

import shutil
import subprocess
from pathlib import Path

import pytest


@pytest.mark.skipif(shutil.which("make") is None, reason="make is unavailable")
def test_light_gate_keeps_chat_rag_memory_and_packaging_without_estimate_suites():
    root = Path(__file__).resolve().parents[1]
    for target in ("verify", "test"):
        output = subprocess.run(["make", "-n", target], cwd=root, check=True,
                                capture_output=True, text=True).stdout
        for required in ("test_light_package.py", "test_light_router_boundary.py",
                         "test_light_chat_evidence.py", "test_retrieval_service.py",
                         "test_memory_core.py", "test_model_connection_chat_integration.py"):
            assert required in output
        assert "test_smeta_chat_application_service.py" not in output
        assert "test_rim_session.py" not in output
        assert "--basetemp=.test-tmp/" in output


def test_makefile_cannot_offer_full_les_operations():
    makefile = (Path(__file__).resolve().parents[1] / "Makefile").read_text(encoding="utf-8")
    assert "smeta" not in makefile.lower()
    assert "deploy-runtime:" not in makefile
    assert "ship:" not in makefile
    assert "release:" not in makefile
