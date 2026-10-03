"""Build offline and web installers from a verified LES payload; never publish."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
INSTALLERS = ROOT / "installers/windows/light"


def verify_installer_description(path: Path) -> None:
    """Check the built PE resource, catching silent ANSI decoding of Russian text."""
    escaped = str(path).replace("'", "''")
    script = ("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); "
              f"[Diagnostics.FileVersionInfo]::GetVersionInfo('{escaped}').FileDescription")
    command = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
    result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", command],
                            check=True, capture_output=True, timeout=30)
    if result.stdout.decode("utf-8-sig").strip() != "Установка LES RAG":
        raise ValueError("Installer Russian resources were decoded incorrectly")


def release_manifest(installer: Path, package: dict) -> dict:
    version = package["version"]
    with installer.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {
        "schema": "les.light-update.v1",
        "application_id": "me.ovc.les-light",
        "version": version,
        "build_number": package["build_number"],
        "bytes": installer.stat().st_size,
        "sha256": digest,
        "installer_url": f"https://github.com/proovcme/LES/releases/download/v{version}/LES-RAG-Setup.exe",
    }


def build(payload: Path, output: Path, makensis: Path) -> None:
    payload, output, makensis = payload.resolve(), output.resolve(), makensis.resolve()
    if output.exists():
        raise FileExistsError("Use a new output directory to preserve previous installers")
    if not makensis.is_file():
        raise FileNotFoundError("An existing NSIS compiler is required")
    package = json.loads((payload / "light-package.json").read_text(encoding="utf-8"))
    version = json.loads((ROOT / "config/version.json").read_text(encoding="utf-8"))
    if package.get("version") != version["product_version"] or package.get("build_number") != version["build_number"]:
        raise ValueError("Payload version differs from the current source contract")
    subprocess.run([
        "powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
        "-File", str(INSTALLERS / "package.ps1"), "-Mode", "Validate", "-Source", str(payload),
    ], check=True)
    output.mkdir(parents=True)
    offline = output / "LES-RAG-Setup.exe"
    subprocess.run([
        str(makensis), "/INPUTCHARSET", "UTF8", f"/DPAYLOAD_DIR={payload}", f"/DOUTPUT_FILE={offline}",
        f"/DPRODUCT_VERSION={package['version']}", str(INSTALLERS / "setup.nsi"),
    ], cwd=INSTALLERS, check=True)
    verify_installer_description(offline)
    subprocess.run([
        str(makensis), "/INPUTCHARSET", "UTF8", f"/DOUTPUT_FILE={output / 'LES-RAG-Web-Setup.exe'}",
        str(INSTALLERS / "bootstrap.nsi"),
    ], cwd=INSTALLERS, check=True)
    shutil.copy2(INSTALLERS / "install-les.ps1", output / "install-les.ps1")
    (output / "light-update.json").write_text(
        json.dumps(release_manifest(offline, package), indent=2) + "\n", encoding="utf-8",
    )
    hashes = []
    for path in sorted(output.iterdir()):
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        hashes.append(f"{digest}  {path.name}")
    (output / "SHA256.txt").write_text("\n".join(hashes) + "\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--makensis", type=Path, required=True)
    args = parser.parse_args()
    build(args.payload, args.output, args.makensis)
