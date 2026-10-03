# Detect the shared Evergreen runtime before replacing LES. No app data is changed.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'

function Test-LesWebViewRuntime {
    foreach ($hive in @([Microsoft.Win32.RegistryHive]::CurrentUser, [Microsoft.Win32.RegistryHive]::LocalMachine)) {
        foreach ($view in @([Microsoft.Win32.RegistryView]::Registry32, [Microsoft.Win32.RegistryView]::Registry64)) {
            $base = $null; $key = $null
            try {
                $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $view)
                $key = $base.OpenSubKey('Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}')
                if ($key) {
                    $version = $null
                    if ([version]::TryParse([string]$key.GetValue('pv'), [ref]$version) -and $version -gt [version]'0.0.0.0') { return $true }
                }
            } catch [System.Security.SecurityException] { continue }
            finally { if ($key) { $key.Dispose() }; if ($base) { $base.Dispose() } }
        }
    }
    return $false
}

function Receive-LesWebViewBootstrap([string]$Destination) {
    Add-Type -AssemblyName System.Net.Http
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $client = New-Object Net.Http.HttpClient
    $cancel = New-Object Threading.CancellationTokenSource
    $cancel.CancelAfter([TimeSpan]::FromMinutes(3))
    $response = $null; $inputStream = $null; $outputStream = $null
    try {
        $response = $client.GetAsync('https://go.microsoft.com/fwlink/p/?LinkId=2124703', [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $cancel.Token).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode() | Out-Null
        if ($response.RequestMessage.RequestUri.Scheme -ne 'https') { throw 'WebView2 download requires HTTPS.' }
        if ($response.Content.Headers.ContentLength -gt 20MB) { throw 'Unexpected WebView2 bootstrap size.' }
        $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $outputStream = [IO.File]::Open($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
        $buffer = New-Object byte[] 65536
        $received = 0L
        while (($count = $inputStream.ReadAsync($buffer, 0, $buffer.Length, $cancel.Token).GetAwaiter().GetResult()) -gt 0) {
            $received += $count
            if ($received -gt 20MB) { throw 'Unexpected WebView2 bootstrap size.' }
            $outputStream.Write($buffer, 0, $count)
        }
        if ($received -lt 1) { throw 'WebView2 download is empty.' }
    } finally {
        if ($outputStream) { $outputStream.Dispose() }; if ($inputStream) { $inputStream.Dispose() }
        if ($response) { $response.Dispose() }; $cancel.Dispose(); $client.Dispose()
    }
}

function Assert-LesMicrosoftSignature([string]$Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid' -or -not $signature.SignerCertificate -or
        $signature.SignerCertificate.Subject -notmatch '(^|,\s*)O=Microsoft Corporation(,|$)') {
        throw 'Microsoft signature could not be verified. WebView2 installer will not run.'
    }
}

function Ensure-LesWebViewRuntime {
    if (Test-LesWebViewRuntime) { Write-Host 'WebView2 runtime is ready.'; return }
    Write-Host 'WebView2 is missing. Downloading the signed Microsoft bootstrapper...'
    $download = Join-Path $PSScriptRoot ('webview-' + [guid]::NewGuid().ToString('N') + '.exe')
    try {
        Receive-LesWebViewBootstrap $download
        Assert-LesMicrosoftSignature $download
        Write-Host 'Installing WebView2. This may take several minutes...'
        $process = Start-Process -FilePath $download -ArgumentList '/silent /install' -WindowStyle Hidden -PassThru
        # Wait with a deadline; do not kill Microsoft's independent updater on timeout.
        if (-not $process.WaitForExit(600000)) { throw 'WebView2 is still installing. Wait for it to finish, then run LES Setup again.' }
        if ($process.ExitCode -ne 0 -or -not (Test-LesWebViewRuntime)) { throw 'WebView2 installation did not complete. Install the Evergreen runtime from Microsoft and retry LES Setup.' }
        Write-Host 'WebView2 runtime is ready.'
    } finally {
        if (Test-Path -LiteralPath $download) { Remove-Item -LiteralPath $download -Force -ErrorAction SilentlyContinue }
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    try { Ensure-LesWebViewRuntime; exit 0 }
    catch { Write-Error $_ -ErrorAction Continue; exit 1 }
}
