"""Stage the pinned native Qdrant asset for LES Light; no Docker or global install."""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
import tarfile
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time
import httpx
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[1]


def host_target() -> str:
    if sys.platform == "darwin":
        machine = platform.machine().lower()
        return {"arm64": "aarch64-apple-darwin", "aarch64": "aarch64-apple-darwin",
                "x86_64": "x86_64-apple-darwin"}[machine]
    if sys.platform == "win32":
        return "x86_64-pc-windows-msvc"
    raise ValueError("Native Light runtime supports Windows and macOS only")


def verified_executable(archive: Path, expected_sha256: str, destination: Path,
                        *, executable_name: str = "qdrant.exe") -> Path:
    with archive.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != expected_sha256:
        raise ValueError("Контрольная сумма Qdrant не совпала. Файл не установлен; повторите загрузку.")
    if executable_name not in {"qdrant", "qdrant.exe"}:
        raise ValueError("Invalid native executable name")
    bundle = ZipFile(archive) if executable_name.endswith(".exe") else tarfile.open(archive, "r:gz")
    temporary = None
    try:
        items = bundle.infolist() if isinstance(bundle, ZipFile) else bundle.getmembers()
        matches = [item for item in items if Path(item.filename if isinstance(bundle, ZipFile)
                   else item.name).name == executable_name]
        if len(matches) != 1 or (not isinstance(bundle, ZipFile) and not matches[0].isfile()):
            raise ValueError("В архиве отсутствует единственный исполняемый файл Qdrant.")
        member = matches[0]
        size = member.file_size if isinstance(bundle, ZipFile) else member.size
        if not 0 < size <= 512 * 1024 * 1024:
            raise ValueError("Invalid native executable size")
        destination.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=destination, suffix=".partial", delete=False) as output:
            temporary = Path(output.name)
            source = bundle.open(member) if isinstance(bundle, ZipFile) else bundle.extractfile(member)
            with source:
                shutil.copyfileobj(source, output)
        if executable_name == "qdrant":
            temporary.chmod(0o755)
        target = destination / executable_name
        temporary.replace(target)
        return target
    finally:
        bundle.close()
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def stage(destination: Path, *, target: str = "x86_64-pc-windows-msvc") -> Path:
    contract = json.loads((ROOT / "config/light-runtime.json").read_text(encoding="utf-8"))["qdrant"]
    if target != "x86_64-pc-windows-msvc":
        asset = contract.get("platforms", {}).get(target)
        if asset is None:
            raise ValueError("Unsupported native Qdrant target")
        contract = {"version": contract["version"], **asset, "target": target}
    else:
        contract = {key: value for key, value in contract.items() if key != "platforms"}
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination) as temporary:
        archive = Path(temporary) / ("qdrant.zip" if target.endswith("msvc") else "qdrant.tar.gz")
        started = time.monotonic()
        with httpx.stream("GET", contract["url"], follow_redirects=True, timeout=20) as source, archive.open("wb") as output:
            source.raise_for_status()
            for chunk in source.iter_bytes(chunk_size=65536):
                if time.monotonic() - started > 180:
                    raise TimeoutError("Загрузка Qdrant превысила 3 минуты. Повторите загрузку; неполный файл не устанавливается.")
                output.write(chunk)
        executable = verified_executable(archive, contract["sha256"], destination, executable_name="qdrant.exe" if target.endswith("msvc") else "qdrant")
    (destination / "asset-receipt.json").write_text(
        json.dumps({**contract, "executable_sha256": hashlib.sha256(executable.read_bytes()).hexdigest()}, indent=2),
        encoding="utf-8",
    )
    return executable


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--target", default=host_target())
    args = parser.parse_args()
    print(stage(args.destination, target=args.target).resolve())
