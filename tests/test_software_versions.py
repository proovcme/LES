from __future__ import annotations

import json
import tomllib
from pathlib import Path

from proxy.services import version_service
from tools import build_tauri_app
from tools import sync_version_contract


ROOT = Path(__file__).resolve().parents[1]


def test_single_product_version_contract_is_consistent():
    contract = json.loads((ROOT / "config/version.json").read_text(encoding="utf-8"))
    assert contract["product_version"] == version_service.PRODUCT_VERSION
    assert contract["build_number"] == version_service.BUILD_NUMBER
    assert contract["desktop_version"] == version_service.DESKTOP_VERSION
    assert build_tauri_app.desktop_semver(
        contract["product_version"], contract["build_number"], edition=contract.get("product_edition", "full")
    ) == contract["desktop_version"]
    assert f'version = "{contract["product_version"]}"' in (ROOT / "pyproject.toml").read_text()
    for relative in ('desktop/light/src-tauri/Cargo.toml', 'desktop/light/src-tauri/Cargo.lock'):
        assert f'version = "{contract["desktop_version"]}"' in (ROOT / relative).read_text(encoding='utf-8-sig')
    assert json.loads((ROOT / 'desktop/light/src-tauri/tauri.conf.json').read_text())['version'] == contract['desktop_version']


def test_qdrant_runtime_is_pinned_everywhere():
    contract = json.loads((ROOT / 'config/light-runtime.json').read_text(encoding='utf-8'))['qdrant']
    assert contract['version'] in contract['url']
    assert 'latest' not in contract['url']
    assert len(contract['sha256']) == 64
    int(contract['sha256'], 16)


def test_software_version_passport_records_required_runtime():
    contract = json.loads((ROOT / "config/version.json").read_text(encoding="utf-8"))
    text = (ROOT / "docs/SOFTWARE_VERSIONS.md").read_text(encoding="utf-8")
    for marker in (
        contract["product_version"], str(contract["build_number"]), contract["desktop_version"],
        "Python", "uv", "Qdrant", "1.19.1", "Ollama", "uv.lock",
    ):
        assert marker in text
    assert f'| Номер сборки | `{contract["build_number"]}` |' in text


def test_version_surfaces_have_no_drift():
    assert sync_version_contract.drifted_surfaces() == []


def test_offline_python_support_matches_bundled_runtime():
    project = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
    lock = tomllib.loads((ROOT / "uv.lock").read_text(encoding="utf-8"))
    bundled = json.loads((ROOT / "config/windows_python.json").read_text(encoding="utf-8"))

    assert project["project"]["requires-python"].replace(" ", "") == ">=3.12,<3.14"
    assert lock["requires-python"].replace(" ", "") == ">=3.12,<3.14"
    assert bundled["version"] == "3.13.12"
