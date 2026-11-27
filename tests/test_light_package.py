import json
from pathlib import Path

from tools import build_light_package as package


def test_publication_scan_rejects_user_paths_on_each_platform(tmp_path):
    from tools.publication_check import sensitive_text_hits

    # Build synthetic paths so the guard also scans this regression itself.
    paths = [
        "/".join(["C:", "Users", "sample-user", "private.txt"]),
        "\\".join(["C:", "Users", "sample-user", "private.txt"]),
        "/".join(["", "Users", "sample-user", "private.txt"]),
        "/".join(["", "home", "sample-user", "private.txt"]),
    ]
    fixture = tmp_path / "fixture.txt"
    fixture.write_text("\n".join(paths), encoding="utf-8")
    assert sensitive_text_hits(tmp_path, [fixture.name]) == [
        (fixture.name, line, "local user path") for line in range(1, 5)
    ]
    fixture.write_text("/usr/share/example\n/Users/\n/home/\n", encoding="utf-8")
    assert sensitive_text_hits(tmp_path, [fixture.name]) == []


def test_windows_native_payload_excludes_application_python(tmp_path):
    package.stage_native_code(tmp_path)
    assert (tmp_path / "les.cmd").read_bytes() == (package.ROOT / "tools/les-native.cmd").read_bytes()
    runtime = tmp_path / "runtime"
    assert (runtime / "tools/light_windows_ocr.ps1").is_file()
    assert (runtime / "tools/install-les.ps1").is_file()
    assert not list(runtime.rglob("*.py"))
    assert not any((runtime / folder).exists() for folder in ("proxy", "backend", "sovushka"))
    assert not (tmp_path / "python").exists()


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
