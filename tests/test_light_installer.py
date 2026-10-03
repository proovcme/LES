import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest


SCRIPT = Path(__file__).resolve().parents[1] / "installers/windows/light/package.ps1"
pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Native PowerShell installer acceptance")


def package(tmp_path, content=b"fixture executable; never executed"):
    source = tmp_path / "payload"
    source.mkdir(exist_ok=True)
    (source / "les-light.exe").write_bytes(content)
    manifest = {"schema": "les.light-package.v1", "application_id": "me.ovc.les-light", "version": "test", "files": [{"path": "les-light.exe", "sha256": hashlib.sha256(content).hexdigest()}]}
    (source / "light-package.json").write_text(json.dumps(manifest))
    return source


def invoke(tmp_path, mode, *args):
    environment = {**os.environ, "LOCALAPPDATA": str(tmp_path / "profile")}
    return subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(SCRIPT), "-Mode", mode, *map(str, args)], env=environment, capture_output=True, timeout=30)


def test_install_update_uninstall_preserves_user_data_and_full_les(tmp_path):
    source = package(tmp_path)
    full = tmp_path / "profile/Programs/LES"
    full.mkdir(parents=True)
    (full / "keep.txt").write_text("full LES")
    result = invoke(tmp_path, "Install", "-Source", source)
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    state = tmp_path / "profile/LES Light"
    (state / "document.txt").write_text("user document")
    source = package(tmp_path, b"updated fixture")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    assert list((tmp_path / "profile/Programs").glob("LES Light.rollback-*"))
    assert invoke(tmp_path, "Remove").returncode == 0
    assert (state / "document.txt").read_text() == "user document"
    assert not (tmp_path / "profile/Programs/LES Light").exists()
    assert not list((tmp_path / "profile/Programs").glob("LES Light.rollback-*"))
    assert (full / "keep.txt").read_text() == "full LES"
    assert invoke(tmp_path, "Remove", "-RemoveUserData").returncode == 0
    assert not state.exists()


def test_corrupt_package_does_not_replace_existing_installation(tmp_path):
    source = package(tmp_path)
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    installed = tmp_path / "profile/Programs/LES Light/les-light.exe"
    expected = installed.read_bytes()
    (source / "les-light.exe").write_bytes(b"corrupt")
    assert invoke(tmp_path, "Install", "-Source", source).returncode != 0
    assert installed.read_bytes() == expected


def test_remover_refuses_unowned_directory(tmp_path):
    target = tmp_path / "profile/Programs/LES Light"
    target.mkdir(parents=True)
    (target / "keep.txt").write_text("not an installation")
    assert invoke(tmp_path, "Remove").returncode != 0
    assert (target / "keep.txt").exists()


def test_unlisted_payload_file_blocks_installation(tmp_path):
    source = package(tmp_path)
    (source / "unexpected.txt").write_text("not in manifest")
    assert invoke(tmp_path, "Install", "-Source", source).returncode != 0
    assert not (tmp_path / "profile/Programs/LES Light").exists()


def test_rollback_restores_old_application_and_keeps_new_user_data(tmp_path):
    source = package(tmp_path, b"original")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    package(tmp_path, b"new revision")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    state = tmp_path / "profile/LES Light"
    (state / "new-document.txt").write_text("created after update")
    assert invoke(tmp_path, "Rollback").returncode == 0
    assert (tmp_path / "profile/Programs/LES Light/les-light.exe").read_bytes() == b"original"
    assert (state / "new-document.txt").read_text() == "created after update"


def test_no_free_space_preserves_installed_version(tmp_path):
    source = package(tmp_path, b"original")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    package(tmp_path, b"new revision")
    # Source the production script through a narrowly instrumented copy: only
    # the OS free-space reading is replaced, the transaction stays unchanged.
    script = tmp_path / "no-space.ps1"
    original = SCRIPT.read_text(encoding="utf-8-sig")
    assert 'return ([IO.DriveInfo]([IO.Path]::GetPathRoot($Path))).AvailableFreeSpace' in original
    script.write_text(original.replace('return ([IO.DriveInfo]([IO.Path]::GetPathRoot($Path))).AvailableFreeSpace', 'return 0'), encoding="utf-8-sig")
    result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(script), "-Mode", "Install", "-Source", str(source)], env={**os.environ, "LOCALAPPDATA": str(tmp_path / "profile")}, capture_output=True, timeout=30)
    assert result.returncode != 0
    assert (tmp_path / "profile/Programs/LES Light/les-light.exe").read_bytes() == b"original"
    assert not list((tmp_path / "profile/Programs").glob("LES Light.staging-*"))


@pytest.mark.parametrize("path", ["../outside.txt", "C:/outside.txt", "data/private.txt", ".env"])
def test_invalid_manifest_paths_are_rejected(tmp_path, path):
    source = package(tmp_path)
    manifest = json.loads((source / "light-package.json").read_text())
    manifest["files"].append({"path": path, "sha256": "0" * 64})
    (source / "light-package.json").write_text(json.dumps(manifest))
    assert invoke(tmp_path, "Validate", "-Source", source).returncode != 0
    assert not (tmp_path / "profile/Programs/LES Light").exists()


def test_locked_installation_is_not_partially_replaced(tmp_path):
    import ctypes
    from ctypes import wintypes
    source = package(tmp_path, b"original")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    target = tmp_path / "profile/Programs/LES Light"
    package(tmp_path, b"new revision")
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.CreateFileW(str(target / "les-light.exe"), 0x80000000, 0, None, 3, 0, None)
    assert handle != ctypes.c_void_p(-1).value
    try:
        result = invoke(tmp_path, "Install", "-Source", source)
        assert result.returncode != 0
    finally:
        kernel.CloseHandle(handle)
    assert (target / "les-light.exe").read_bytes() == b"original"


def test_activation_failure_restores_old_directory(tmp_path):
    source = package(tmp_path, b"original")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    package(tmp_path, b"new revision")
    script = tmp_path / "activation-failure.ps1"
    original = SCRIPT.read_text(encoding="utf-8-sig")
    operation = 'Move-Item -LiteralPath $staging -Destination $installRoot; $swapped = $true'
    assert operation in original
    script.write_text(original.replace(operation, "throw 'injected activation failure'"), encoding="utf-8-sig")
    result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(script), "-Mode", "Install", "-Source", str(source)], env={**os.environ, "LOCALAPPDATA": str(tmp_path / "profile")}, capture_output=True, timeout=30)
    assert result.returncode != 0
    assert (tmp_path / "profile/Programs/LES Light/les-light.exe").read_bytes() == b"original"
    assert not list((tmp_path / "profile/Programs").glob("LES Light.staging-*"))


def test_uninstaller_refuses_junction_and_preserves_its_target(tmp_path):
    source = package(tmp_path)
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    foreign = tmp_path / "foreign"
    foreign.mkdir()
    (foreign / "keep.txt").write_text("outside installation")
    link = tmp_path / "profile/Programs/LES Light/linked"
    script = tmp_path / "junction.ps1"
    script.write_text('param([string]$Link, [string]$Target)\nNew-Item -ItemType Junction -Path $Link -Target $Target | Out-Null\n')
    result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(script), "-Link", str(link), "-Target", str(foreign)], capture_output=True, timeout=20)
    assert result.returncode == 0
    assert invoke(tmp_path, "Remove").returncode != 0
    assert (foreign / "keep.txt").read_text() == "outside installation"


def test_recovery_after_interruption_between_renames(tmp_path):
    source = package(tmp_path, b"original")
    assert invoke(tmp_path, "Install", "-Source", source).returncode == 0
    root = tmp_path / "profile/Programs/LES Light"
    backup = root.with_name("LES Light.rollback-interrupted")
    staging = root.with_name("LES Light.staging-interrupted")
    root.rename(backup)
    staging.mkdir()
    (staging / "light-install.json").write_text(json.dumps({"application_id": "me.ovc.les-light"}))
    (staging / "les-light.exe").write_bytes(b"uncommitted")
    state = tmp_path / "profile/LES Light"
    (state / "document.txt").write_text("preserve me")
    (state / "install-transaction.json").write_text(json.dumps({"application_id": "me.ovc.les-light", "backup": str(backup), "staging": str(staging)}))
    result = invoke(tmp_path, "Recover")
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    assert (root / "les-light.exe").read_bytes() == b"original"
    assert (state / "document.txt").read_text() == "preserve me"
    assert not staging.exists()
    assert not (state / "install-transaction.json").exists()


def test_parallel_installer_is_rejected_before_mutation(tmp_path):
    source = package(tmp_path)
    holder = tmp_path / "hold-mutex.ps1"
    holder.write_text("$name = 'Global\\LES.Light.Install.' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value\n$mutex = New-Object Threading.Mutex($false, $name)\n$null = $mutex.WaitOne(0)\n[Console]::WriteLine('locked')\n$null = [Console]::ReadLine()\n$mutex.ReleaseMutex()\n")
    process = subprocess.Popen(["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(holder)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        assert process.stdout.readline().strip() == b"locked"
        assert invoke(tmp_path, "Install", "-Source", source).returncode != 0
        assert not (tmp_path / "profile/Programs/LES Light").exists()
    finally:
        process.communicate(b"release\n", timeout=10)


def test_install_and_remove_under_unicode_profile_with_spaces(tmp_path):
    profile = tmp_path / "Пользователь с пробелами"
    profile.mkdir()
    source = package(profile)
    result = invoke(profile, "Install", "-Source", source)
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    assert (profile / "profile/Programs/LES Light/les-light.exe").is_file()
    assert invoke(profile, "Remove").returncode == 0


def test_windows_write_denial_does_not_damage_existing_files(tmp_path):
    source = package(tmp_path)
    target = tmp_path / "profile/Programs"
    target.mkdir(parents=True)
    sentinel = target / "keep.txt"
    sentinel.write_text("untouched")
    identity = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value"], capture_output=True, check=True, timeout=15).stdout.decode().strip()
    assert identity.startswith("S-1-")
    principal = "*" + identity
    # Deny writes only; preserve permission-management rights for cleanup.
    subprocess.run(["icacls.exe", str(target), "/deny", principal + ":(OI)(CI)(WD,AD,WEA,WA)"], capture_output=True, check=True, timeout=15)
    try:
        assert invoke(tmp_path, "Install", "-Source", source).returncode != 0
    finally:
        subprocess.run(["icacls.exe", str(target), "/remove:d", principal], capture_output=True, check=True, timeout=15)
    assert sentinel.read_text() == "untouched"
    assert not (target / "LES Light").exists()


@pytest.mark.parametrize("mode", ["Install", "Remove", "Recover", "Rollback"])
def test_system_directory_override_is_rejected_before_any_changes(tmp_path, mode):
    source = package(tmp_path)
    # Guard executes before any target creation/deletion. No ACL changes or
    # test files are made in real system directories.
    system_root = os.environ.get("SystemRoot", r"C:\Windows")
    result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-File", str(SCRIPT), "-Mode", mode, "-Source", str(source)], env={**os.environ, "LOCALAPPDATA": system_root}, capture_output=True, timeout=15)
    assert result.returncode != 0
    assert "Program Files" in result.stderr.decode("utf-8", errors="replace")


def test_desktop_installer_is_per_user_and_registers_only_own_application():
    source = (SCRIPT.parent / "setup.nsi").read_text(encoding="utf-8")
    assert "RequestExecutionLevel user" in source
    assert "WriteRegStr HKLM" not in source
    assert "me.ovc.les-light" in source
    assert "Programs\\LES Light" in source
    assert "taskkill" not in source
    dialogs = [line for line in source.splitlines() if line.strip().startswith("MessageBox ")]
    assert dialogs
    assert all("/SD IDOK" in line for line in dialogs), "Silent failures must not wait for an invisible dialog"
