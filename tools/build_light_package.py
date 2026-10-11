"""Build a relocatable offline Light payload from the existing frozen uv.lock."""
from __future__ import annotations

import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import zipfile

from tools.build_release_artifacts import iter_files
from tools.build_tauri_app import stage_windows_python, stage_windows_uv, windows_python_contract
from tools.light_runtime_assets import stage as stage_qdrant

ROOT = Path(__file__).resolve().parents[1]
CODE_PREFIXES = ("backend/", "proxy/", "sovushka/", "schema/")
# These routers are selected only by the full-LES branch in proxy.app. Keep the
# list explicit: broad name-based pruning can remove shared Light dependencies.
FULL_LES_ONLY_CODE = frozenset({
    "schema/smeta_agent_trace.schema.json",
    "tools/les_mcp_server.py",
    "proxy/routers/lsr.py",
    "proxy/routers/rim.py",
    "proxy/routers/service_sources.py",
    "proxy/routers/updates.py",
    "proxy/routers/speckle.py",
})
FULL_LES_ONLY_PREFIXES = (
    "proxy/smeta_core/",
    "proxy/services/smeta_",
    "proxy/services/rim_",
    "proxy/services/lsr_",
    "proxy/services/gesn_",
    "proxy/services/fgis_",
    "tools/smeta_",
    "tools/build_smeta_",
    "tools/activate_smeta_",
    "tools/publish_smeta_",
    "tools/rebuild_active_smeta_",
    "tools/fgis_",
    "tools/gesn_",
)
FULL_LES_ONLY_SERVICES = frozenset({
    "proxy/services/ks2_xlsx_render.py",
    "proxy/services/ks_forms_chat_service.py",
    "proxy/services/ks_forms_service.py",
    "proxy/services/les_action_service.py",
    "proxy/services/estimate_harness_service.py",
    "proxy/services/estimate_math_service.py",
    "proxy/services/memory_smeta_observer.py",
    "proxy/services/service_source_registry.py",
})


def _full_les_only(relative: str) -> bool:
    return (relative in FULL_LES_ONLY_CODE or relative in FULL_LES_ONLY_SERVICES
            or relative.startswith(FULL_LES_ONLY_PREFIXES))
CODE_FILES = {"sovushka_ng.py", "pyproject.toml", "uv.lock", "tools/__init__.py", "tools/light_launcher.py", "tools/light_mcp_server.py", "tools/light_child.py",
              "tools/light_windows_ocr.ps1", "tools/light_cli.py", "tools/les-light.cmd",
              "tools/backup_suharik.py", "tools/les_doctor.py", "tools/les_runtime_control.py", "tools/lesctl.py",
              "proxy/services/mcp_connection_service.py", "proxy/routers/mcp_connections.py",
                "backend/local_reranker.py", "backend/index_replacement.py", "backend/sparse_index.py",
                "backend/bm25_store.py", "backend/bm25_hybrid.py", "backend/sparse_legacy.py",
                "backend/inference/bm25_weighted.py",
                "tools/rebuild_sparse_index.py",
              "proxy/services/chat_section_context_service.py",
              "proxy/services/document_task_store.py", "proxy/services/table_document_tool.py",
              "proxy/services/tabular_document_service.py",
              "sovushka/components/mcp_connections.py", "sovushka/components/light_shell.py"}
CONFIG_FILES = {"version.json", "light-runtime.json", "windows_python.json", "windows_uv.json"}
PUBLIC_ASSETS = {"tools/light_macos_ocr.swift",
    "qdrant_visualizer/index.html", "qdrant_visualizer/forest.js",
    "qdrant_visualizer/forest-model.js", "qdrant_visualizer/forest.css",
    "qdrant_visualizer/navigation.js",
    "qdrant_visualizer/forest-mist.svg",
    "frontend/pwa/manifest.webmanifest", "frontend/pwa/offline.html",
    "frontend/pwa/service-worker.js",
    "sovushka/uikit/styles/00_base.css", "sovushka/uikit/styles/01_chat.css",
    "sovushka/uikit/styles/02_tools.css", "sovushka/uikit/styles/03_settings.css",
    "sovushka/uikit/styles/04_lists.css", "sovushka/uikit/styles/05_documents.css",
    "sovushka/uikit/styles/06_workspace.css", "sovushka/uikit/styles/07_forest.css",
}


def _local_untracked_candidate_files() -> set[Path]:
    """Allow a complete local candidate before its first authorized git commit.

    Only known source roots and named public assets are eligible. In particular,
    data, storage, credentials, logs, attachment trees and symlinks are excluded.
    A public release still requires the separate publication audit.
    """
    root = ROOT.resolve()
    candidates: set[Path] = set()
    for prefix in CODE_PREFIXES:
        directory = ROOT / prefix
        if not directory.is_dir():
            continue
        for source in directory.rglob("*"):
            if source.is_symlink() or not source.is_file():
                continue
            if not source.resolve().is_relative_to(root):
                continue
            if source.suffix == ".py" or prefix == "schema/" and source.suffix == ".json":
                candidates.add(source)
    tools_root = ROOT / "tools"
    if tools_root.is_dir():
        candidates.update(
            source for source in tools_root.rglob("*.py")
            if source.is_file() and not source.is_symlink()
            and source.resolve().is_relative_to(root)
        )
    for name in PUBLIC_ASSETS:
        source = ROOT / name
        if source.is_file() and not source.is_symlink():
            candidates.add(source)
    for name in CONFIG_FILES:
        source = ROOT / "config" / name
        if source.is_file() and not source.is_symlink():
            candidates.add(source)
    return candidates


def hash_file(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stage_code(payload):
    runtime = payload / "runtime"
    tracked = set(iter_files())
    tracked.update(_local_untracked_candidate_files())
    # Explicit source list supports a local candidate before owner-authorized git publication.
    tracked.update(ROOT / name for name in CODE_FILES if (ROOT / name).is_file())
    sources = set()
    for source in tracked:
        relative = source.relative_to(ROOT).as_posix()
        if _full_les_only(relative):
            continue
        if relative.startswith(CODE_PREFIXES) or relative in CODE_FILES or relative in PUBLIC_ASSETS or relative.startswith("config/") and source.name in CONFIG_FILES:
            sources.add(source)
    pending = list(sources)
    while pending:
        source = pending.pop()
        if source.suffix != ".py":
            continue
        for node in ast.walk(ast.parse(source.read_text(encoding="utf-8-sig"))):
            names = [alias.name for alias in node.names] if isinstance(node, ast.Import) else [node.module or ""] if isinstance(node, ast.ImportFrom) else []
            if isinstance(node, ast.ImportFrom) and node.module == "tools":
                names = [f"tools.{alias.name}" for alias in node.names]
            for name in names:
                if not name.startswith("tools."):
                    continue
                dependency = ROOT.joinpath(*name.split(".")).with_suffix(".py")
                if dependency in tracked and dependency not in sources and not _full_les_only(dependency.relative_to(ROOT).as_posix()):
                    sources.add(dependency)
                    pending.append(dependency)
    for source in sorted(sources):
        target = runtime / source.relative_to(ROOT)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
    for name in ("LICENSE", "README.md", "USER_GUIDE.md", "BACKLOG.md"):
        source = ROOT / "docs/public/les-light" / name
        shutil.copy2(source, payload / name)
    # The runtime pyproject readme is present even though no project install is required.
    (payload / "assets").mkdir(exist_ok=True)
    shutil.copy2(ROOT / "docs/public/les-light/assets/les-light.svg", payload / "assets/les-light.svg")
    shutil.copy2(payload / "README.md", runtime / "README.md")
    shutil.copy2(ROOT / 'tools/les-light.cmd', payload / 'les.cmd')
    return runtime


def remove_build_assets(payload, archive_name):
    bundled_tools = payload / "runtime/installers/windows/tools"
    for name in ("uv.exe", archive_name):
        (bundled_tools / name).unlink()


def stage_dependencies(payload, runtime):
    stage_windows_python(runtime)
    stage_windows_uv(runtime)
    bundled_tools = runtime / "installers/windows/tools"
    python_dir = payload / "python"
    python_dir.mkdir(exist_ok=True)
    contract = windows_python_contract()
    with zipfile.ZipFile(bundled_tools / contract["archive_name"]) as archive:
        # Archive is pinned and verified by stage_windows_python.
        for name in archive.namelist():
            if Path(name).is_absolute() or ".." in Path(name).parts:
                raise ValueError("Unsafe Python archive member")
        archive.extractall(python_dir)
    uv = bundled_tools / "uv.exe"
    requirements = payload / "locked-requirements.txt"
    subprocess.run([str(uv), "export", "--frozen", "--extra", "mcp", "--no-dev", "--no-emit-project", "--format", "requirements-txt", "--output-file", str(requirements)], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
    subprocess.run([str(uv), "pip", "sync", str(requirements), "--python", str(python_dir / "python.exe"),
        "--target", str(python_dir / "Lib/site-packages"), "--no-python-downloads", "--require-hashes"], check=True)
    for pth in python_dir.glob("python*._pth"):
        pth.write_text("python313.zip\n.\nLib/site-packages\n../runtime\nimport site\n", encoding="utf-8")
    subprocess.run([str(python_dir / "python.exe"), "-B", "-c", "import fastapi, nicegui, qdrant_client, pandas, numpy, sqlite3; from backend.light_processes import InstanceLock; print('bundled runtime import check passed')"], cwd=payload, check=True)
    # These pinned assets build the embedded environment. The installed app
    # runs python/python.exe directly and must not ship a second Python archive
    # or the build machine's dependency installer.
    remove_build_assets(payload, contract["archive_name"])


def write_manifest(payload):
    version = json.loads((ROOT / "config/version.json").read_text(encoding="utf-8"))
    files = []
    for path in sorted(payload.rglob("*")):
        if path.is_file() and path != payload / "light-package.json":
            relative = path.relative_to(payload).as_posix()
            # Bytecode is disposable and must not embed build-machine paths.
            if "__pycache__" in path.parts or path.suffix == ".pyc":
                path.unlink()
                continue
            files.append({"path": relative, "sha256": hash_file(path)})
    result = {"schema": "les.light-package.v1", "application_id": "me.ovc.les-light", "version": version["product_version"],
              "build_number": version["build_number"], "uv_lock_sha256": hash_file(ROOT / "uv.lock"), "files": files}
    (payload / "light-package.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    return result


def stage_mail_collector(payload):
    """Compile on the build host; users never need a compiler or source checkout."""
    windows = Path(os.environ.get("SystemRoot", "C:/Windows"))
    candidates = [windows / f"Microsoft.NET/{arch}/v4.0.30319/csc.exe" for arch in ("Framework64", "Framework")]
    compiler = next((path for path in candidates if path.is_file()), None)
    if compiler is None:
        raise FileNotFoundError("The Windows build host needs the .NET Framework C# compiler for the Outlook collector")
    target = payload / "native/mail/LesLightMailPoller.exe"
    target.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run([str(compiler), "/nologo", "/target:winexe", f"/out:{target}",
                    "/r:System.dll", "/r:System.Core.dll", "/r:Microsoft.CSharp.dll",
                    str(ROOT / "clients/outlook_mail_poller/LesMailPoller.cs")], check=True)
    if not target.is_file():
        raise FileNotFoundError("The Outlook collector build did not produce its executable")
    return target


def build(destination, executable, qdrant=None):
    destination = destination.resolve()
    if destination.exists():
        raise FileExistsError("Use a new empty output directory; existing artifacts are never removed by this builder")
    if not executable.is_file():
        raise FileNotFoundError("Build the dedicated desktop/light shell first")
    destination.mkdir(parents=True)
    shutil.copy2(executable, destination / "les-light.exe")
    runtime = stage_code(destination)
    stage_dependencies(destination, runtime)
    native = destination / "native/qdrant"
    if qdrant:
        receipt = qdrant.parent / "asset-receipt.json"
        expected = json.loads(receipt.read_text(encoding="utf-8"))
        pinned = json.loads((ROOT / "config/light-runtime.json").read_text(encoding="utf-8"))["qdrant"]
        if expected.get("sha256") != pinned["sha256"] or hash_file(qdrant) != expected.get("executable_sha256"):
            raise ValueError("Qdrant asset does not match the pinned receipt")
        native.mkdir(parents=True)
        shutil.copy2(qdrant, native / "qdrant.exe")
        shutil.copy2(receipt, native / "asset-receipt.json")
    else:
        stage_qdrant(native)
    stage_mail_collector(destination)
    from tools.light_license_audit import stage_notices
    stage_notices(ROOT, destination)
    return write_manifest(destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--desktop-exe", type=Path, required=True)
    parser.add_argument("--qdrant", type=Path)
    args = parser.parse_args()
    result = build(args.output, args.desktop_exe.resolve(), args.qdrant.resolve() if args.qdrant else None)
    print(json.dumps({"files": len(result["files"]), "version": result["version"], "output": str(args.output)}))
