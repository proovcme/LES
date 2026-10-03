import json
import subprocess
from pathlib import Path

import pytest

from tools.code_runtime_map import _tracked_python_files, build_inventory, render_markdown


ROOT = Path(__file__).resolve().parents[1]


def test_tracked_python_files_ignore_worktree_deletions(tmp_path: Path):
    subprocess.run(["git", "init", "--quiet"], cwd=tmp_path, check=True)
    keep = tmp_path / "keep.py"
    retired = tmp_path / "retired.py"
    keep.write_text("VALUE = 1\n", encoding="utf-8")
    retired.write_text("VALUE = 2\n", encoding="utf-8")
    subprocess.run(["git", "add", "keep.py", "retired.py"], cwd=tmp_path, check=True)
    retired.unlink()

    assert _tracked_python_files(tmp_path) == ["keep.py"]


def test_runtime_map_ignores_local_pytest_and_package_stages(tmp_path: Path):
    subprocess.run(["git", "init", "--quiet"], cwd=tmp_path, check=True)
    for name in ("keep.py", ".pytest_tmp/copy.py", ".test-tmp/runtime/proxy/app.py"):
        path = tmp_path / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("VALUE = 1\n", encoding="utf-8")
    assert _tracked_python_files(tmp_path) == ["keep.py"]


def _modules_by_path(inventory: dict) -> dict[str, dict]:
    return {item["path"]: item for item in inventory["modules"]}


@pytest.fixture(scope="module")
def inventory() -> dict:
    return build_inventory(ROOT)


def test_runtime_map_distinguishes_product_runtime_and_tool_only_code(inventory: dict):
    modules = _modules_by_path(inventory)

    assert modules["proxy/app.py"]["status"] == "PRODUCT_REACHABLE"
    assert modules["proxy/routers/chat.py"]["status"] == "PRODUCT_REACHABLE"
    assert not any("smeta" in path for path, item in modules.items()
                   if item["status"] == "PRODUCT_REACHABLE")
    assert modules["tools/windows_update_engine.py"]["status"] == "RUNTIME_SUPPORT"
    assert modules["tools/build_tauri_app.py"]["status"] == "TEST_OR_TOOL_ONLY"


def test_runtime_map_exposes_chat_without_estimate_routes(inventory: dict):
    paths = {route["path"] for route in inventory["routes"]}
    assert "/api/chat" in paths
    assert not any(path.startswith(("/api/smeta-", "/api/rim")) for path in paths)
    assert "proxy/routers/chat.py" in inventory["focus"]


def test_runtime_map_is_conservative_and_deterministically_sorted(inventory: dict):
    paths = [item["path"] for item in inventory["modules"]]
    route_keys = [
        (item["path"], item["method"], item["source"], item["handler"])
        for item in inventory["routes"]
    ]

    assert paths == sorted(paths)
    assert route_keys == sorted(route_keys)
    # The public Light tree deliberately excludes archived full-product tests.
    assert inventory["summary"]["tracked_python_files"] >= inventory["summary"]["product_reachable"]
    assert {"proxy/app.py", "proxy/routers/chat.py", "sovushka_ng.py"}.issubset(paths)
    assert inventory["summary"]["product_reachable"] > 100
    assert inventory["summary"]["dormant_candidates"] == sum(item["status"] == "DORMANT_CANDIDATE" for item in inventory["modules"])
    markdown = render_markdown(inventory)
    assert "не является доказательством мёртвого кода" in markdown
    assert "proxy/routers/chat.py" in markdown
    assert "DORMANT_CANDIDATE" in markdown
    assert (ROOT / "docs" / "CODE_RUNTIME_MAP.md").read_text(encoding="utf-8") == markdown
    assert json.loads(
        (ROOT / "docs" / "generated" / "code_runtime_map.json").read_text(encoding="utf-8")
    ) == inventory


def test_light_mail_surface_is_reachable_after_activation(
    inventory: dict,
):
    modules = _modules_by_path(inventory)
    assert modules["sovushka/pages/mail.py"]["status"] == "PRODUCT_REACHABLE"
    for retired in (
        "proxy/legacy_app.py",
        "test_auth.py",
        "test_ng.py",
        "tools/pikabu_construction_rd.py",
        "tools/test_chunk_density.py",
    ):
        assert retired not in modules


def test_retired_one_off_operator_scripts_do_not_reenter_tool_inventory(
    inventory: dict,
):
    modules = _modules_by_path(inventory)

    for supported in (
        "proxy/routers/checklist_review.py",
        "sovushka/pages/samovar.py",
        "tools/ezhik_imap_smoke.py",
        "tools/light_launcher.py",
    ):
        assert supported in modules

    for retired in (
        "tools/checklist_review_smoke.py",
        "tools/ezhik_mail_smoke.py",
        "tools/rag_batch_parse.py",
        "tools/rebucket_ntd_other.py",
        "tools/smart_dataset_plan.py",
        "tools/smart_dataset_rebuild.py",
    ):
        assert retired not in modules


def test_api_surface_keeps_profiles_and_internal_extraction_without_duplicate_public_controls(
    inventory: dict,
):
    modules = _modules_by_path(inventory)
    routes = {(item["method"], item["path"]) for item in inventory["routes"]}

    assert "proxy/services/prompt_registry_service.py" in modules
    assert "proxy/services/extract_service.py" in modules
    assert ("GET", "/api/profiles") in routes

    assert ("GET", "/api/prompts") not in routes
    assert ("PATCH", "/api/prompts/{prompt_key:path}") not in routes
    assert ("DELETE", "/api/prompts/{prompt_key:path}") not in routes
    assert ("POST", "/api/extract/structured") not in routes


def test_unconsumed_experimental_api_is_retired_without_removing_active_workflows(
    inventory: dict,
):
    modules = _modules_by_path(inventory)
    paths = {item["path"] for item in inventory["routes"]}

    for active_prefix in (
        "/api/tasks",
        "/api/notes",
        "/api/filemap",
    ):
        assert any(path == active_prefix or path.startswith(f"{active_prefix}/") for path in paths)

    for retired_prefix in (
        "/api/bor",
        "/api/decisions",
        "/api/doc-review",
        "/api/edges",
        "/api/estimates",
        "/api/field",
        "/api/kac",
        "/api/les-md",
        "/api/ontology",
        "/api/prices",
    ):
        assert not [
            path
            for path in paths
            if path == retired_prefix or path.startswith(f"{retired_prefix}/")
        ]

    for retired_module in (
        "proxy/routers/bor.py",
        "proxy/routers/decisions.py",
        "proxy/routers/doc_review.py",
        "proxy/routers/edges.py",
        "proxy/routers/estimates.py",
        "proxy/routers/field.py",
        "proxy/routers/kac.py",
        "proxy/routers/les_md.py",
        "proxy/routers/ontology.py",
        "proxy/routers/prices.py",
        "proxy/routers/status_page.py",
    ):
        assert retired_module not in modules

    assert "proxy/routers/incoming_control.py" not in modules
    assert "proxy/services/incoming_control_service.py" not in modules
    assert not [path for path in paths if path.startswith("/api/incoming-control")]


def test_unfinished_worklog_and_field_api_are_retired_without_removing_glossary(
    inventory: dict,
):
    modules = _modules_by_path(inventory)
    paths = {item["path"] for item in inventory["routes"]}

    assert not [path for path in paths if path == "/api/field" or path.startswith("/api/field/")]
    assert "proxy/services/glossary_chat_service.py" in modules
    assert "proxy/routers/worklog.py" not in modules
    assert "proxy/services/work_log_service.py" not in modules
    assert not [path for path in paths if path.startswith("/api/worklog")]


def test_unintegrated_diff_and_cad_bim_are_absent_from_light(
    inventory: dict,
):
    modules = _modules_by_path(inventory)
    paths = {item["path"] for item in inventory["routes"]}

    assert not any(path.startswith("/api/cad-bim") for path in paths)
    assert "proxy/routers/diff.py" not in modules
    assert "proxy/services/diff_service.py" not in modules
    assert not [path for path in paths if path.startswith("/api/diff")]


def test_legacy_normcontrol_and_unconsumed_doc_review_apis_are_retired(
    inventory: dict,
):
    paths = {item["path"] for item in inventory["routes"]}

    assert "/api/doc-review/{dataset_id}/run" not in paths
    assert not [path for path in paths if path.startswith("/api/normcontrol")]
