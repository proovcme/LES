# LES RAG online installer. Requires Windows PowerShell 5.1 or newer.
[CmdletBinding()]
param(
    [switch]$Silent,
    [switch]$DownloadOnly,
    [string]$OutputDirectory = ''
)

$ErrorActionPreference = 'Stop'

function ConvertTo-LesRelease([object]$Manifest) {
    if ($Manifest.schema -ne 'les.light-update.v1' -or $Manifest.application_id -ne 'me.ovc.les-light') {
        throw 'This release does not belong to LES RAG.'
    }
    if ([string]$Manifest.version -notmatch '^\d+\.\d+\.\d+\z' -or
        [string]$Manifest.sha256 -cnotmatch '^[0-9a-f]{64}\z' -or
        $Manifest.bytes -isnot [ValueType] -or $Manifest.bytes -is [bool] -or
        [double]$Manifest.bytes -ne [math]::Floor([double]$Manifest.bytes) -or
        $Manifest.bytes -lt 1 -or $Manifest.bytes -gt 1073741824 -or
        $Manifest.build_number -isnot [ValueType] -or $Manifest.build_number -is [bool] -or
        [double]$Manifest.build_number -ne [math]::Floor([double]$Manifest.build_number) -or
        $Manifest.build_number -lt 1) {
        throw 'The release manifest has an invalid version, size, build or SHA-256.'
    }
    return [pscustomobject]@{
        Version = [string]$Manifest.version
        Build = [long]$Manifest.build_number
        Bytes = [long]$Manifest.bytes
        Sha256 = [string]$Manifest.sha256
        Uri = [uri]("https://github.com/proovcme/LES/releases/download/v{0}/LES-RAG-Setup.exe" -f $Manifest.version)
    }
}

function New-LesHttpClient {
    Add-Type -AssemblyName System.Net.Http
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $client = New-Object System.Net.Http.HttpClient
    $client.Timeout = [TimeSpan]::FromSeconds(90)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('LES-RAG-Installer/1.0')
    return $client
}

function Get-LesRelease([object]$Client, [uri]$Uri) {
    $cancel = New-Object Threading.CancellationTokenSource
    $cancel.CancelAfter([TimeSpan]::FromSeconds(60))
    $response = $null
    $stream = $null
    try {
        $response = $Client.GetAsync($Uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $cancel.Token).GetAwaiter().GetResult()
        if ([int]$response.StatusCode -eq 404) { throw 'A public LES RAG release has not been published yet.' }
        $response.EnsureSuccessStatusCode() | Out-Null
        if ($response.Content.Headers.ContentLength -gt 65536) { throw 'Release manifest is too large.' }
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $memory = New-Object IO.MemoryStream
        try {
            $buffer = New-Object byte[] 4096
            while (($count = $stream.ReadAsync($buffer, 0, $buffer.Length, $cancel.Token).GetAwaiter().GetResult()) -gt 0) {
                if ($memory.Length + $count -gt 65536) { throw 'Release manifest is too large.' }
                $memory.Write($buffer, 0, $count)
            }
            $utf8 = New-Object Text.UTF8Encoding($false, $true)
            $manifest = $utf8.GetString($memory.ToArray()) | ConvertFrom-Json
            return ConvertTo-LesRelease $manifest
        } finally { $memory.Dispose() }
    } finally {
        if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }
        $cancel.Dispose()
    }
}

function Receive-LesInstaller([object]$Client, [object]$Release, [string]$Directory) {
    $directoryPath = [IO.Path]::GetFullPath($Directory)
    New-Item -ItemType Directory -Path $directoryPath -Force | Out-Null
    $drive = New-Object IO.DriveInfo([IO.Path]::GetPathRoot($directoryPath))
    if ($drive.AvailableFreeSpace -lt $Release.Bytes + 52428800) { throw 'Not enough free disk space for the installer.' }
    $downloadPath = Join-Path $directoryPath ('download-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $downloadPath | Out-Null
    $partial = Join-Path $downloadPath 'package.part'
    $target = Join-Path $downloadPath 'LES-RAG-Setup.exe'
    $response = $null; $stream = $null; $output = $null
    $cancel = New-Object Threading.CancellationTokenSource
    $cancel.CancelAfter([TimeSpan]::FromMinutes(20))
    try {
        $response = $Client.GetAsync($Release.Uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead, $cancel.Token).GetAwaiter().GetResult()
        $response.EnsureSuccessStatusCode() | Out-Null
        if ($null -ne $response.Content.Headers.ContentLength -and $response.Content.Headers.ContentLength -ne $Release.Bytes) {
            throw 'Installer size does not match the release manifest.'
        }
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output = [IO.File]::Open($partial, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        $buffer = New-Object byte[] 1048576
        $received = 0L; $lastProgress = -1
        while (($count = $stream.ReadAsync($buffer, 0, $buffer.Length, $cancel.Token).GetAwaiter().GetResult()) -gt 0) {
            $received += $count
            if ($received -gt $Release.Bytes) { throw 'Installer is larger than the release manifest allows.' }
            $output.Write($buffer, 0, $count)
            $progress = [int][math]::Floor(100 * $received / $Release.Bytes / 5) * 5
            if ($progress -ne $lastProgress) { Write-Host "Downloading LES RAG: $progress%"; $lastProgress = $progress }
        }
        $output.Dispose(); $output = $null
        if ($received -ne $Release.Bytes) { throw 'Download was interrupted. Run the installer again to retry.' }
        $algorithm = [Security.Cryptography.SHA256]::Create()
        $hashStream = [IO.File]::OpenRead($partial)
        try { $actualHash = [BitConverter]::ToString($algorithm.ComputeHash($hashStream)).Replace('-', '').ToLowerInvariant() }
        finally { $hashStream.Dispose(); $algorithm.Dispose() }
        if ($actualHash -cne $Release.Sha256) { throw 'SHA-256 verification failed. The downloaded file will not run.' }
        Move-Item -LiteralPath $partial -Destination $target
        return $target
    } finally {
        if ($output) { $output.Dispose() }
        if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }
        $cancel.Dispose()
        # Only the exact partial file created above; never remove an arbitrary tree.
        if (Test-Path -LiteralPath $partial) { Remove-Item -LiteralPath $partial -Force }
    }
}

function Install-LesRelease {
    $client = New-LesHttpClient
    try {
        Write-Host 'Checking the public LES RAG release...'
        $release = Get-LesRelease $client ([uri]'https://github.com/proovcme/LES/releases/latest/download/light-update.json')
        $directory = $OutputDirectory
        if (-not $directory) {
            if (-not $env:LOCALAPPDATA) { throw 'Windows local user profile is unavailable.' }
            $directory = Join-Path $env:LOCALAPPDATA 'LES Light\install-cache'
        }
        Write-Host ("LES RAG {0}, build {1}. Download: {2:N0} MB." -f $release.Version, $release.Build, ($release.Bytes / 1MB))
        $installer = Receive-LesInstaller $client $release $directory
        Write-Host "Verified installer: $installer"
        if ($DownloadOnly) { return }
        $options = @{ FilePath = $installer; PassThru = $true; Wait = $true }
        if ($Silent) { $options.ArgumentList = '/S'; $options.WindowStyle = 'Hidden' }
        $process = Start-Process @options
        if ($process.ExitCode -ne 0) { throw "LES RAG installation failed (exit $($process.ExitCode)). Close LES and retry. The verified installer is saved above." }
        Write-Host 'LES RAG is installed. Open LES RAG from the Start menu.'
    } finally { $client.Dispose() }
}

# Dot-sourcing exposes functions for offline acceptance without starting installation.
if ($MyInvocation.InvocationName -ne '.') {
    try { Install-LesRelease; exit 0 }
    catch { Write-Error $_ -ErrorAction Continue; exit 1 }
}
