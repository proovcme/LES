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
from urllib.request import urlopen
from zipfile import ZipFile

ROOT = Path(__file__).resolve().parents[1]


def verified_executable(archive: Path, expected_sha256: str, destination: Path) -> Path:
    with archive.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != expected_sha256:
        raise ValueError("Контрольная сумма Qdrant не совпала. Файл не установлен; повторите загрузку.")
    executable_name = "qdrant.exe"
    temporary = None
    try:
        with ZipFile(archive) as bundle:
            matches = [entry for entry in bundle.infolist() if Path(entry.filename).name == executable_name]
            if len(matches) != 1 or matches[0].is_dir():
                raise ValueError("В архиве отсутствует единственный исполняемый файл Qdrant.")
            if not 0 < matches[0].file_size <= 512 * 1024 * 1024:
                raise ValueError("Invalid native executable size")
            destination.mkdir(parents=True, exist_ok=True)
            with bundle.open(matches[0]) as source, tempfile.NamedTemporaryFile(
                dir=destination, suffix=".partial", delete=False,
            ) as output:
                temporary = Path(output.name)
                shutil.copyfileobj(source, output)
        target = destination / executable_name
        temporary.replace(target)
        return target
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def stage(destination: Path) -> Path:
    contract = json.loads((ROOT / "config/light-runtime.json").read_text(encoding="utf-8"))["qdrant"]
    destination.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination) as temporary:
        archive = Path(temporary) / "qdrant.zip"
        started = time.monotonic()
        with urlopen(contract["url"], timeout=20) as source, archive.open("wb") as output:
            while chunk := source.read(65536):
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
