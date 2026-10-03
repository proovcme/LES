"""The Light API cannot expose the inherited estimate write endpoints."""
import json
import os
from pathlib import Path
import subprocess
import sys

from proxy.app import _light_chat_router
from proxy.routers.chat import router as full_chat_router
from proxy.services.tool_registry_service import canonical_tool_registry


def test_light_router_keeps_chat_without_estimate_actions():
    full_paths = {route.path for route in full_chat_router.routes}
    light_paths = {route.path for route in _light_chat_router().routes}
    assert not any("smeta" in path for path in full_paths | light_paths)
    assert {"/api/chat", "/api/chat/stream", "/api/commands"} <= light_paths


def test_light_tool_registry_does_not_offer_workbook_builders():
    names = {item.contract.name for item in canonical_tool_registry().registrations()}
    assert "build_lsr_workbook" not in names
    assert "build_vor_workbook" not in names


def test_light_startup_keeps_full_les_routes_and_smeta_imports_out():
    script = (
        "import json, sys; from proxy.app import app; "
        "print(json.dumps({'paths': sorted({r.path for r in app.routes}), "
        "'smeta_imports': sorted(n for n in sys.modules if n.startswith(('proxy.', 'backend.')) "
        "and ('smeta' in n or n in {'proxy.routers.lsr', 'proxy.routers.rim'}))}))"
    )
    env = os.environ.copy()
    env["LES_PRODUCT_EDITION"] = "light"
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=Path(__file__).resolve().parents[1],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    audit = json.loads(result.stdout.splitlines()[-1])
    paths = audit["paths"]
    forbidden = ("/api/lsr", "/api/rim", "/api/service-sources", "/api/cad-bim",
                 "/api/smeta-", "/api/memory/smeta-traces", "/api/settings/mlx-model",
                 "/api/runtime/dispatcher/mlx/unload")
    assert not [path for path in paths if path.startswith(forbidden)]
    assert audit["smeta_imports"] == []
    assert "/api/chat" in paths and "/api/rag/datasets" in paths


def test_full_les_edition_is_rejected_by_light_repository():
    script = (
        "import json; from proxy.app import app; "
        "print(json.dumps(sorted({r.path for r in app.routes})))"
    )
    env = os.environ.copy()
    env["LES_PRODUCT_EDITION"] = "full"
    result = subprocess.run(
        [sys.executable, "-c", script],
        cwd=Path(__file__).resolve().parents[1],
        env=env,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert "This repository contains LES Light only" in result.stderr
