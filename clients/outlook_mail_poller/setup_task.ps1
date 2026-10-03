# Build/install the classic-Outlook E.ZH.I.K. sidecar and interactive task.
param(
  [string]$InstallRoot = "",
  [string]$StateRoot = "",
  [ValidateSet("full", "light")][string]$Edition = "full",
  [switch]$Probe,
  [switch]$Remove
)
$ErrorActionPreference = "Stop"
$task = "LES E.ZH.I.K. Outlook Collector"
$application = if ($Edition -eq "light") { "LES Light" } else { "LES" }
if ($Edition -eq "light") { $task = "LES Light Outlook Collector" }
if (-not $InstallRoot) { $InstallRoot = Join-Path (Join-Path $env:LOCALAPPDATA $application) "bin" }
if (-not $StateRoot) { $StateRoot = Join-Path (Join-Path $env:LOCALAPPDATA $application) "mail" }

if ($Remove) {
  schtasks /delete /tn $task /f 2>$null
  exit 0
}

$sourceRoot = Split-Path -Parent $MyInvocation.MyCommand.Definition
$source = Join-Path $sourceRoot "LesMailPoller.cs"
$compiler = "C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if (-not (Test-Path -LiteralPath $compiler)) {
  $compiler = "C:\Windows\Microsoft.NET\Framework\v4.0.30319\csc.exe"
}
if (-not (Test-Path -LiteralPath $compiler)) { throw ".NET Framework csc.exe not found" }

New-Item -ItemType Directory -Force -Path $InstallRoot, $StateRoot | Out-Null
$binaryName = if ($Edition -eq "light") { "LesLightMailPoller.exe" } else { "LesMailPoller.exe" }
$target = Join-Path $InstallRoot $binaryName
& $compiler /nologo /target:winexe /out:"$target" /r:System.dll /r:System.Core.dll /r:Microsoft.CSharp.dll $source
if ($LASTEXITCODE -ne 0) { throw "LesMailPoller compile failed ($LASTEXITCODE)" }

if ($Edition -eq "full") {
  "http://127.0.0.1:8050/api/mail/collector/import" |
    Set-Content -LiteralPath (Join-Path $StateRoot "collector_url.txt") -Encoding ASCII
}
# Light writes its active API address when collection is requested in the app.

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $target
$principal = New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive
$settings = New-ScheduledTaskSettingsSet `
  -ExecutionTimeLimit (New-TimeSpan -Seconds 20) `
  -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $task -Action $action -Principal $principal -Settings $settings -Force |
  Out-Null

if ($Probe) {
  & $target --probe
  if ($LASTEXITCODE -ne 0) { throw "Outlook probe failed ($LASTEXITCODE)" }
}

[ordered]@{
  task = $task
  executable = $target
  schedule = "manual"
  interactive_user = $identity
  state_root = $StateRoot
} | ConvertTo-Json -Compress
