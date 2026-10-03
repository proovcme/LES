import json
import os
import subprocess
import sys
from pathlib import Path

from tools import build_light_package as package


def test_finished_package_drops_build_tools_but_keeps_embedded_python(tmp_path):
    assets = tmp_path / "runtime/installers/windows/tools"
    assets.mkdir(parents=True)
    for name in ("uv.exe", "python.zip", "asset-receipt.json"):
        (assets / name).write_bytes(b"build asset")
    (tmp_path / "python").mkdir()
    python = tmp_path / "python/python.exe"
    python.write_bytes(b"runtime")
    package.remove_build_assets(tmp_path, "python.zip")
    assert sorted(path.name for path in assets.iterdir()) == ["asset-receipt.json"]
    assert python.read_bytes() == b"runtime"


def test_package_follows_transitive_tool_imports_without_private_state(tmp_path, monkeypatch):
    source = tmp_path / "source"
    files = {
        "proxy/app.py": "from tools import first\n",
        "proxy/routers/lsr.py": "LEGACY = True\n",
        "proxy/routers/rim.py": "LEGACY = True\n",
        "proxy/routers/service_sources.py": "LEGACY = True\n",
        "proxy/routers/updates.py": "LEGACY = True\n",
        "proxy/routers/speckle.py": "LEGACY = True\n",
        "proxy/smeta_core/workflow.py": "LEGACY = True\n",
        "proxy/services/smeta_chat_service.py": "LEGACY = True\n",
        "proxy/services/estimate_harness_service.py": "LEGACY = True\n",
        "tools/build_smeta_norm_rag.py": "LEGACY = True\n",
        "tools/first.py": "from tools.second import run\n",
        "tools/les-light.cmd": "@echo off\n",
        "tools/second.py": "def run(): pass\n",
        "tools/private.py": "secret = 1\n",
        "data/private.txt": "private",
        "config/version.json": "{}",
        "qdrant_visualizer/index.html": '<script src="forest.js"></script>',
        "qdrant_visualizer/forest.js": "export const forest = true;",
        "qdrant_visualizer/forest-model.js": "export const model = true;",
        "qdrant_visualizer/forest.css": "body {}",
        "qdrant_visualizer/forest-mist.svg": '<svg xmlns="http://www.w3.org/2000/svg"/>',
        "qdrant_visualizer/private-export.json": '{"private":true}',
    }
    for name, contents in files.items():
        path = source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
    for name in ("LICENSE", "README.md", "USER_GUIDE.md", "BACKLOG.md"):
        path = source / "docs/public/les-light" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(name, encoding="utf-8")
    monkeypatch.setattr(package, "ROOT", source)
    logo = source / "docs/public/les-light/assets/les-light.svg"
    logo.parent.mkdir(parents=True)
    logo.write_text("<svg/>")
    monkeypatch.setattr(package, "iter_files", lambda: [source / name for name in files])
    destination = tmp_path / "package"
    runtime = package.stage_code(destination)
    assert (runtime / "tools/second.py").is_file()
    assert not (runtime / "tools/private.py").exists()
    assert not (runtime / "data").exists()
    assert (destination / "assets/les-light.svg").is_file()
    for name in ("index.html", "forest.js", "forest-model.js", "forest.css", "forest-mist.svg"):
        assert (runtime / "qdrant_visualizer" / name).is_file()
    assert not (runtime / "qdrant_visualizer/private-export.json").exists()
    assert not any((runtime / name).exists() for name in package.FULL_LES_ONLY_CODE)
    assert not any((runtime / name).exists() for name in files if package._full_les_only(name))


def test_untracked_local_candidate_contains_runtime_but_no_private_tree(tmp_path, monkeypatch):
    source = tmp_path / "source"
    files = {
        "proxy/app.py": "from tools import first\n",
        "backend/product_edition.py": "LIGHT = True\n",
        "tools/les-light.cmd": "@echo off\n",
        "sovushka/components/light_shell.py": "def build_light_shell(): pass\n",
        "schema/runtime.schema.json": "{}",
        "schema/smeta_agent_trace.schema.json": "{}",
        "tools/first.py": "from tools.second import run\n",
        "tools/second.py": "def run(): pass\n",
        "tools/private.py": "secret = 1\n",
        "data/private.txt": "private",
        "config/version.json": "{}",
    }
    for name, contents in files.items():
        path = source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
    for name in ("LICENSE", "README.md", "USER_GUIDE.md", "BACKLOG.md"):
        path = source / "docs/public/les-light" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(name, encoding="utf-8")
    logo = source / "docs/public/les-light/assets/les-light.svg"
    logo.parent.mkdir(parents=True)
    logo.write_text("<svg/>", encoding="utf-8")
    monkeypatch.setattr(package, "ROOT", source)
    monkeypatch.setattr(package, "iter_files", lambda: [])
    runtime = package.stage_code(tmp_path / "package")
    for name in ("proxy/app.py", "backend/product_edition.py", "sovushka/components/light_shell.py",
                 "schema/runtime.schema.json", "tools/first.py", "tools/second.py"):
        assert (runtime / name).is_file()
    assert not (runtime / "tools/private.py").exists()
    assert not (runtime / "data").exists()
    assert not (runtime / "schema/smeta_agent_trace.schema.json").exists()


def test_manifest_hashes_nested_manifest_and_omits_bytecode(tmp_path, monkeypatch):
    source = tmp_path / "source"
    (source / "config").mkdir(parents=True)
    (source / "config/version.json").write_text(json.dumps({"product_version": "0.1.0", "build_number": 715}))
    (source / "uv.lock").write_text("locked")
    monkeypatch.setattr(package, "ROOT", source)
    payload = tmp_path / "payload"
    (payload / "nested").mkdir(parents=True)
    (payload / "nested/light-package.json").write_text("component")
    (payload / "compiled.pyc").write_bytes(b"disposable")
    (payload / "light-package.json").write_text("previous")
    result = package.write_manifest(payload)
    assert [entry["path"] for entry in result["files"]] == ["nested/light-package.json"]
    assert result["files"][0]["sha256"] == package.hash_file(payload / "nested/light-package.json")
    assert not (payload / "compiled.pyc").exists()


def test_real_light_stage_has_no_estimate_code_and_starts(tmp_path):
    runtime = package.stage_code(tmp_path / "package")
    files = [path.relative_to(runtime).as_posix() for path in runtime.rglob("*") if path.is_file()]
    assert not [path for path in files if package._full_les_only(path)]
    assert not [path for path in files if "smeta" in path or "estimate_" in path]
    env = os.environ.copy()
    env["LES_PRODUCT_EDITION"] = "light"
    result = subprocess.run(
        [sys.executable, "-B", "-c",
         "import proxy.app; print(proxy.app.__file__); print(len(proxy.app.app.routes))"],
        cwd=runtime,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    assert Path(result.stdout.splitlines()[0]).resolve() == (runtime / "proxy/app.py").resolve()
