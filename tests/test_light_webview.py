"""Exercise installer prerequisite decisions without installing system software."""
from pathlib import Path
import subprocess
import sys
import pytest

SCRIPT = Path('installers/windows/light/ensure-webview.ps1').resolve()
pytestmark = pytest.mark.skipif(sys.platform != 'win32', reason='Windows installer')


def run(body):
    source = ". '" + str(SCRIPT).replace("'", "''") + "'; " + body
    return subprocess.run(['powershell.exe','-NoProfile','-NonInteractive','-Command',source],
                          capture_output=True, timeout=25)


def test_existing_runtime_does_not_download_or_start_installer():
    result = run("function Test-LesWebViewRuntime { $true }; function Receive-LesWebViewBootstrap { throw 'unexpected download' }; Ensure-LesWebViewRuntime")
    assert result.returncode == 0, result.stderr


@pytest.mark.parametrize('status,subject', [('NotSigned','O=Microsoft Corporation'), ('Valid','O=Someone Else')])
def test_unsigned_or_foreign_bootstrap_cannot_run(status, subject):
    result = run(f"function Get-AuthenticodeSignature {{ [pscustomobject]@{{Status='{status}';SignerCertificate=[pscustomobject]@{{Subject='{subject}'}}}} }}; try {{ Assert-LesMicrosoftSignature 'fixture'; exit 7 }} catch {{ exit 0 }}")
    assert result.returncode == 0, result.stderr


def test_valid_microsoft_signature_is_accepted():
    result = run("function Get-AuthenticodeSignature { [pscustomobject]@{Status='Valid';SignerCertificate=[pscustomobject]@{Subject='CN=Microsoft Corporation, O=Microsoft Corporation, C=US'}} }; Assert-LesMicrosoftSignature 'fixture'")
    assert result.returncode == 0, result.stderr


def test_missing_runtime_installs_only_after_signature_and_verifies_result():
    result = run("""
    $script:checks=0; $script:verified=$false
    function Test-LesWebViewRuntime { $script:checks++; return $script:checks -gt 1 }
    function Receive-LesWebViewBootstrap { }
    function Assert-LesMicrosoftSignature { $script:verified=$true }
    function Start-Process {
        if (-not $script:verified) { throw 'signature was skipped' }
        $p=[pscustomobject]@{ExitCode=0}
        $p | Add-Member ScriptMethod WaitForExit { param($ms) return $true }
        return $p
    }
    Ensure-LesWebViewRuntime
    if ($script:checks -ne 2) { exit 9 }
    """)
    assert result.returncode == 0, result.stderr


def test_runtime_preflight_precedes_payload_replacement():
    source = Path('installers/windows/light/setup.nsi').read_text(encoding='utf-8')
    assert source.index('ensure-webview.ps1') < source.index('-Mode Install')
