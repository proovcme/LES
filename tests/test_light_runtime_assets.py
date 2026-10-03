import hashlib
from zipfile import ZipFile

import pytest

from tools.light_runtime_assets import verified_executable


def test_archive_integrity_is_checked_before_install(tmp_path):
    archive = tmp_path / "q.zip"
    with ZipFile(archive, "w") as out:
        out.writestr("qdrant.exe", b"test executable")
    destination = tmp_path / "app"
    with pytest.raises(ValueError, match="Контрольная сумма"):
        verified_executable(archive, "0" * 64, destination)
    assert not destination.exists()


def test_only_executable_is_extracted_to_fixed_destination(tmp_path):
    archive = tmp_path / "q.zip"
    with ZipFile(archive, "w") as out:
        out.writestr("../../qdrant.exe", b"test executable")
        out.writestr("../unrelated.txt", b"not an application file")
    destination = tmp_path / "app"
    path = verified_executable(archive, hashlib.sha256(archive.read_bytes()).hexdigest(), destination)
    assert path == destination / "qdrant.exe"
    assert path.read_bytes() == b"test executable"
    assert not (tmp_path / "unrelated.txt").exists()
