"""Build a relocatable offline Light payload from the existing frozen uv.lock."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import zipfile

from tools.light_runtime_assets import stage as stage_qdrant

ROOT = Path(__file__).resolve().parents[1]


def hash_file(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stage_native_code(payload):
    runtime = payload / "runtime"
    for name in ("tools/light_windows_ocr.ps1", "config/version.json"):
        target = runtime / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / name, target)
    shutil.copy2(ROOT / "installers/windows/light/install-les.ps1", runtime / "tools/install-les.ps1")
    public = ROOT / "docs/public/les-light"
    for name in ("LICENSE", "README.md", "USER_GUIDE.md", "BACKLOG.md"):
        shutil.copy2(public / name, payload / name)
    (payload / "assets").mkdir()
    shutil.copy2(public / "assets/les-light.svg", payload / "assets/les-light.svg")
    shutil.copy2(ROOT / "tools/les-native.cmd", payload / "les.cmd")
    from tools.build_light_frontend import stage as stage_frontend
    stage_frontend(ROOT, runtime)


def stage_skill_dependencies(destination, source=None):
    from tools.skill_python_asset import seal, stage, stage_build_inputs
    lock_hash = hash_file(ROOT / "uv.lock")
    if source is not None:
        sealed = Path(source) / "skill-python-receipt.json"
        if sealed.is_file():
            previous = json.loads(sealed.read_text(encoding="utf-8"))
            if previous.get("schema") != "les.skill-python.v1" or previous.get("source_uv_lock_sha256") != lock_hash:
                raise ValueError("Prepared skill runtime was sealed against a different lock")
            destination.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(dir=destination.parent) as scratch:
                checked = Path(scratch) / "skill-python"
                receipt = stage(checked, source, lock_sha256=lock_hash)
                if receipt["inventory_sha256"] != previous.get("inventory_sha256"):
                    raise ValueError("Prepared skill runtime differs from its sealed files")
                checked.rename(destination)
        else:
            receipt = stage(destination, source, lock_sha256=lock_hash)
    else:
        with tempfile.TemporaryDirectory(prefix="les-build-tools-") as scratch:
            scratch = Path(scratch)
            python_archive, uv = stage_build_inputs(scratch)
            with zipfile.ZipFile(python_archive) as archive:
                for name in archive.namelist():
                    if Path(name).is_absolute() or ".." in Path(name).parts:
                        raise ValueError("Unsafe Python archive member")
                archive.extractall(destination)
            requirements = scratch / "locked-requirements.txt"
            subprocess.run([str(uv), "export", "--frozen", "--only-group", "skill-runtime",
                "--no-emit-project", "--format", "requirements-txt",
                "--output-file", str(requirements)], cwd=ROOT, check=True, stdout=subprocess.DEVNULL)
            subprocess.run([str(uv), "pip", "sync", str(requirements), "--python", str(destination / "python.exe"),
                "--target", str(destination / "Lib/site-packages"), "--no-python-downloads", "--require-hashes",
                "--no-compile-bytecode"], check=True)
        receipt = seal(destination, next(destination.glob("python*.zip")).stem, lock_sha256=lock_hash)
    if any(item["name"].lower() == "nicegui" for item in receipt["distributions"]):
        raise ValueError("The skill runtime must not contain NiceGUI")
    if (destination / "Lib/site-packages/nicegui").exists():
        raise ValueError("The skill runtime must not contain NiceGUI sources")
    return receipt


def skill_asset_path(payload, build_number):
    return payload.with_name(f"{payload.name}-assets") / f"LES-Skill-Python-{build_number}.zip"


def stage_skill_asset(payload, runtime, version):
    artifact = skill_asset_path(payload, version["build_number"])
    artifact.parent.mkdir(parents=True, exist_ok=True)
    with artifact.open("xb") as output, zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(runtime.rglob("*")):
            if path.is_file():
                member = zipfile.ZipInfo(path.relative_to(runtime).as_posix())
                member.compress_type = zipfile.ZIP_DEFLATED
                with path.open("rb") as source, archive.open(member, "w") as target:
                    shutil.copyfileobj(source, target)
    descriptor = {
        "schema": "les.skill-python-download.v1",
        "bytes": artifact.stat().st_size,
        "sha256": hash_file(artifact),
        "receipt_sha256": hash_file(runtime / "skill-python-receipt.json"),
    }
    (payload / "native/skill-python-download.json").write_text(
        json.dumps(descriptor, indent=2) + "\n", encoding="utf-8"
    )


def stage_pdfium(payload, source):
    if not source.is_file():
        raise FileNotFoundError("Provide the PDFium DLL from the locked pypdfium2 wheel")
    target = payload / "native/pdf/pdfium.dll"
    target.parent.mkdir(parents=True)
    shutil.copy2(source, target)
    from tools.light_license_audit import collect
    collect(source.parent.parent, payload / "THIRD_PARTY_NOTICES/native/pdfium", only="pypdfium2")


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
    result = {"schema": "les.light-package.v1", "application_id": "me.ovc.les-light", "runtime_mode": "rust", "version": version["product_version"],
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


def build(destination, executable, qdrant=None, native_runtime=None, native_api=None, skill_python=None, native_parsers=None, engineering_readers=None):
    destination = destination.resolve()
    if destination.exists():
        raise FileExistsError("Use a new empty output directory; existing artifacts are never removed by this builder")
    if not executable.is_file():
        raise FileNotFoundError("Build the dedicated desktop/light shell first")
    destination.mkdir(parents=True)
    shutil.copy2(executable, destination / "les-light.exe")
    stage_native_code(destination)
    version = json.loads((ROOT / "config/version.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="les-skill-component-", dir=destination.parent) as scratch:
        skill_root = Path(scratch) / "skill-python"
        stage_skill_dependencies(skill_root, skill_python)
        stage_pdfium(destination, skill_root / "Lib/site-packages/pypdfium2_raw/pdfium.dll")
        from tools.light_license_audit import stage_skill_notices
        stage_skill_notices(skill_root)
        stage_skill_asset(destination, skill_root, version)
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
    from tools.build_native_runtime import build as build_runtime, stage_asset as stage_runtime
    if native_runtime is None:
        build_runtime(destination/'native/runtime')
    else:
        stage_runtime(destination/'native/runtime', native_runtime)
    if native_api is not None:
        from tools.build_native_api import stage_asset as stage_api
        stage_api(destination/'native/api', native_api)
    else:
        from tools.build_native_api import build as build_api
        build_api(destination/'native/api')
    from tools import native_asset
    if native_parsers is None:
        native_asset.build('parsers',destination/'native/parsers')
    else:
        native_asset.stage_asset('parsers',destination/'native/parsers',native_parsers,native_asset.source_digest('parsers'))
    if engineering_readers is not None:
        from tools.engineering_reader_assets import stage as stage_engineering_readers
        stage_engineering_readers(engineering_readers, destination / 'native/engineering-readers')
    stage_mail_collector(destination)
    from tools.light_license_audit import stage_notices
    stage_notices(ROOT, destination)
    return write_manifest(destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--desktop-exe", type=Path, required=True)
    parser.add_argument("--qdrant", type=Path)
    parser.add_argument("--native-runtime", type=Path)
    parser.add_argument("--native-api", type=Path)
    parser.add_argument("--native-parsers", type=Path)
    parser.add_argument("--engineering-readers", type=Path, help="Prepared standalone DWG and RVT readers with third-party notices")
    parser.add_argument("--skill-python", type=Path, help="Prepared portable skill runtime; defaults to the locked bundled dependency set")
    args = parser.parse_args()
    result = build(args.output, args.desktop_exe.resolve(), args.qdrant.resolve() if args.qdrant else None,
                   args.native_runtime.resolve() if args.native_runtime else None,
                   args.native_api.resolve() if args.native_api else None,
                   args.skill_python.resolve() if args.skill_python else None,
                   args.native_parsers.resolve() if args.native_parsers else None,
                   args.engineering_readers.resolve() if args.engineering_readers else None)
    print(json.dumps({"files": len(result["files"]), "version": result["version"], "output": str(args.output)}))
