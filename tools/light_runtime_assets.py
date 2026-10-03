"""Stage the pinned native Qdrant asset for LES Light; no Docker or global install."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import time
import httpx
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[1]


def verified_executable(archive: Path, expected_sha256: str, destination: Path) -> Path:
    with archive.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != expected_sha256:
        raise ValueError("Контрольная сумма Qdrant не совпала. Файл не установлен; повторите загрузку.")
    with ZipFile(archive) as bundle:
        executables = [item for item in bundle.infolist() if Path(item.filename).name.lower() == "qdrant.exe"]
        if len(executables) != 1:
            raise ValueError("В архиве отсутствует единственный qdrant.exe. Файл не установлен.")
        destination.mkdir(parents=True, exist_ok=True)
        target = destination / "qdrant.exe"
        # Extract a single verified member to our fixed name, never archive paths.
        with tempfile.NamedTemporaryFile(dir=destination, suffix=".partial", delete=False) as output:
            temporary = Path(output.name)
            try:
                with bundle.open(executables[0]) as source:
                    shutil.copyfileobj(source, output)
            except BaseException:
                output.close()
                temporary.unlink(missing_ok=True)
                raise
        try:
            temporary.replace(target)
        finally:
            temporary.unlink(missing_ok=True)
    return target


def stage(destination: Path) -> Path:
    contract = json.loads((ROOT / "config/light-runtime.json").read_text(encoding="utf-8"))["qdrant"]
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination) as temporary:
        archive = Path(temporary) / "qdrant.zip"
        started = time.monotonic()
        with httpx.stream("GET", contract["url"], follow_redirects=True, timeout=20) as source, archive.open("wb") as output:
            source.raise_for_status()
            for chunk in source.iter_bytes(chunk_size=65536):
                if time.monotonic() - started > 180:
                    raise TimeoutError("Загрузка Qdrant превысила 3 минуты. Повторите загрузку; неполный файл не устанавливается.")
                output.write(chunk)
        executable = verified_executable(archive, contract["sha256"], destination)
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
    args = parser.parse_args()
    print(stage(args.destination).resolve())
