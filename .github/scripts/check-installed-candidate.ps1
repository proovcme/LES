param(
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][ValidateSet('fresh', 'upgrade')][string]$Scenario
)
$ErrorActionPreference = 'Stop'
$app = Join-Path $env:LOCALAPPDATA 'Programs/LES Light'
$state = Join-Path $env:LOCALAPPDATA 'LES Light'
$expected = Get-Content -LiteralPath 'config/version.json' -Raw | ConvertFrom-Json
if ((Test-Path -LiteralPath $app) -or (Test-Path -LiteralPath $state)) {
    throw 'This check requires a fresh Windows runner.'
}

function Install-Les([string]$Directory, [int]$Build) {
    $setup = Join-Path $Directory 'LES-RAG-Setup.exe'
    $checksum = Get-Content -LiteralPath (Join-Path $Directory 'SHA256.txt') |
        Where-Object { $_ -match '^([0-9a-f]{64})\s+LES-RAG-Setup\.exe$' }
    if (@($checksum).Count -ne 1 -or (Get-FileHash -LiteralPath $setup).Hash -ne ($checksum -split '\s+')[0]) {
        throw 'Setup checksum mismatch.'
    }
    $installer = Start-Process -FilePath $setup -ArgumentList '/S' -WindowStyle Hidden -PassThru -Wait
    if ($installer.ExitCode -ne 0) { throw "Setup exit code $($installer.ExitCode)." }
    $installed = Get-Content -LiteralPath (Join-Path $app 'runtime/config/version.json') -Raw | ConvertFrom-Json
    if ($installed.build_number -ne $Build) { throw "Expected installed build $Build." }
}

function Invoke-InstalledLes([scriptblock]$Check) {
    $info = [Diagnostics.ProcessStartInfo]::new((Join-Path $app 'native/runtime/les-runtime.exe'))
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.CreateNoWindow = $true
    $info.WorkingDirectory = $app
    foreach ($arg in @('--native-only', '--root', (Join-Path $app 'runtime'), '--state', $state,
                       '--qdrant', (Join-Path $app 'native/qdrant/qdrant.exe'), '--parent-pipe')) {
        $info.ArgumentList.Add($arg)
    }
    $info.Environment['PATH'] = "$env:SystemRoot\System32;$env:SystemRoot"
    $null = $info.Environment.Remove('PYTHONHOME')
    $null = $info.Environment.Remove('PYTHONPATH')
    $runtime = [Diagnostics.Process]::Start($info)
    try {
        $statusPath = Join-Path $state 'launcher-status.json'
        $deadline = [datetime]::UtcNow.AddSeconds(90)
        do {
            if ($runtime.HasExited) { throw "Installed runtime exited with code $($runtime.ExitCode)." }
            $status = Get-Content -LiteralPath $statusPath -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json
            if ($status.phase -eq 'ready') { break }
            Start-Sleep -Seconds 1
        } while ([datetime]::UtcNow -lt $deadline)
        if ($status.phase -ne 'ready') { throw "Installed runtime did not become ready ($($status.phase))." }
        $address = [uri]$status.api_url
        if ($address.Scheme -ne 'http' -or $address.Host -ne '127.0.0.1') { throw 'Expected local API.' }
        $base = $address.GetLeftPart([UriPartial]::Authority)
        $null = Invoke-WebRequest -Uri "$base/app/" -TimeoutSec 10
        & $Check $base
    } finally {
        if (-not $runtime.HasExited) {
            $runtime.StandardInput.WriteLine('stop')
            $runtime.StandardInput.Close()
            if (-not $runtime.WaitForExit(20000)) { throw 'Installed runtime did not stop on request.' }
            if ($runtime.ExitCode -ne 0) { throw "Installed runtime stop exit code $($runtime.ExitCode)." }
        }
        $runtime.Dispose()
    }
}

function Read-SmokeSession([string]$Base, [string]$SessionId) {
    $row = Invoke-RestMethod -Uri "$Base/api/workspace/sessions/$SessionId" -TimeoutSec 10
    if ($row.title -ne 'Installed acceptance') { throw 'Session did not survive restart.' }
}

if ($Scenario -eq 'upgrade') {
    $baseline = New-Item -ItemType Directory -Path (Join-Path $env:RUNNER_TEMP 'les-baseline')
    $url = 'https://github.com/proovcme/LES/releases/download/v0.1.2'
    foreach ($asset in @('LES-RAG-Setup.exe', 'SHA256.txt')) {
        Invoke-WebRequest -Uri "$url/$asset" -OutFile (Join-Path $baseline.FullName $asset) -TimeoutSec 120
    }
    Install-Les $baseline.FullName 914
} else {
    Install-Les $Candidate $expected.build_number
}

$sessionId = Invoke-InstalledLes {
    param($base)
    $row = Invoke-RestMethod -Uri "$base/api/workspace/sessions" -Method Post -Headers @{Origin=$base} `
        -ContentType 'application/json' -Body '{"title":"Installed acceptance"}' -TimeoutSec 10
    if (-not $row.session_id) { throw 'Installed runtime did not create a session.' }
    $row.session_id
}

if ($Scenario -eq 'upgrade') { Install-Les $Candidate $expected.build_number }
Invoke-InstalledLes {
    param($base)
    Read-SmokeSession $base $sessionId
    $headers = @{Origin=$base; 'Idempotency-Key'=[Guid]::NewGuid().ToString()}
    $body = @{session_id=$sessionId; question='/help'} | ConvertTo-Json
    $first = Invoke-RestMethod -Uri "$base/api/chat" -Method Post -Headers $headers -ContentType 'application/json' -Body $body
    $again = Invoke-RestMethod -Uri "$base/api/chat" -Method Post -Headers $headers -ContentType 'application/json' -Body $body
    if (-not $first.history_id -or $first.history_id -ne $again.history_id -or $first.answer -ne $again.answer) {
        throw 'Installed chat did not replay the committed answer.'
    }
}

if ($Scenario -eq 'upgrade') {
    & installers/windows/light/package.ps1 -Mode Rollback
    $restored = Get-Content -LiteralPath (Join-Path $app 'runtime/config/version.json') -Raw | ConvertFrom-Json
    if ($restored.build_number -ne 914) { throw 'Rollback did not restore build 914.' }
    Invoke-InstalledLes { param($base) Read-SmokeSession $base $sessionId }
}
