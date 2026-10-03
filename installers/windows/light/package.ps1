# Transactional installation and explicit data removal for the public edition.
[CmdletBinding()]
param(
  [Parameter(Mandatory=$true)][ValidateSet('Validate', 'Install', 'Remove', 'Rollback', 'Recover')][string]$Mode,
  [string]$Source = '',
  [switch]$RemoveUserData
)
$ErrorActionPreference = 'Stop'
$appId = 'me.ovc.les-light'
if (-not $env:LOCALAPPDATA) { throw 'Windows не сообщил папку локальных данных пользователя. Войдите в свой профиль Windows и повторите запуск.' }
$profileBase = [IO.Path]::GetFullPath($env:LOCALAPPDATA).TrimEnd('\')
if ($profileBase.StartsWith('\\') -or $profileBase -eq [IO.Path]::GetPathRoot($profileBase).TrimEnd('\')) {
  throw 'LES Light устанавливается в локальный профиль пользователя, а не в корень диска или сетевую папку.'
}
foreach ($special in @([Environment+SpecialFolder]::Windows, [Environment+SpecialFolder]::System, [Environment+SpecialFolder]::ProgramFiles, [Environment+SpecialFolder]::ProgramFilesX86)) {
  $protected = [Environment]::GetFolderPath($special).TrimEnd('\')
  if ($protected -and ($profileBase.Equals($protected, [StringComparison]::OrdinalIgnoreCase) -or $profileBase.StartsWith($protected + '\', [StringComparison]::OrdinalIgnoreCase))) {
    throw 'LES Light не устанавливает и не удаляет файлы в системных каталогах Windows и Program Files. Используйте обычный профиль пользователя.'
  }
}
$installRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs\LES Light'))
$stateRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'LES Light'))

function Get-PackageHash([string]$Path) {
  $algorithm = [Security.Cryptography.SHA256]::Create()
  $stream = [IO.File]::OpenRead($Path)
  try { return [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
  finally { $stream.Dispose(); $algorithm.Dispose() }
}

function Get-FreeBytes([string]$Path) {
  return ([IO.DriveInfo]([IO.Path]::GetPathRoot($Path))).AvailableFreeSpace
}

function Assert-AppStopped([string]$Path) {
  $binary = Join-Path $Path 'les-light.exe'
  if (Test-Path -LiteralPath $binary) {
    try {
      $probe = [IO.File]::Open($binary, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
      $probe.Dispose()
    } catch [IO.IOException] {
      throw 'LES Light сейчас используется. Закройте его окна и повторите действие. Другие приложения останавливать не нужно.'
    }
  }
}

function Assert-PlainTree([string]$Path) {
  if (-not (Test-Path -LiteralPath $Path)) { return }
  $pending = New-Object 'System.Collections.Generic.Stack[string]'
  $pending.Push([IO.Path]::GetFullPath($Path))
  while ($pending.Count -gt 0) {
    $entry = Get-Item -LiteralPath $pending.Pop() -Force
    if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
      throw "LES Light: каталог содержит ссылку на другое место. Удаление или замена отменены: $($entry.FullName)"
    }
    if ($entry.PSIsContainer) {
      foreach ($child in Get-ChildItem -LiteralPath $entry.FullName -Force) { $pending.Push($child.FullName) }
    }
  }
}

function Assert-Marker([string]$Path) {
  $marker = Join-Path $Path 'light-install.json'
  if (-not (Test-Path -LiteralPath $marker)) { throw "LES Light: каталог не принадлежит этой установке: $Path" }
  $identity = Get-Content -LiteralPath $marker -Raw | ConvertFrom-Json
  if ($identity.application_id -ne $appId) { throw "LES Light: неверная принадлежность каталога: $Path" }
}

function Assert-Parents([string]$Path) {
  $entry = [IO.DirectoryInfo]([IO.Path]::GetFullPath($Path))
  while ($entry) {
    if ($entry.Exists -and (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
      throw 'LES Light: установка через перенаправленный каталог запрещена. Выберите обычный профиль Windows.'
    }
    $entry = $entry.Parent
  }
}

Assert-Parents $installRoot
Assert-Parents $stateRoot
if ($Mode -ne 'Validate') {
  $mutexName = 'Global\LES.Light.Install.' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $installationMutex = New-Object Threading.Mutex($false, $mutexName)
  try {
    if (-not $installationMutex.WaitOne(0)) { throw 'Установка или удаление LES Light уже выполняется. Дождитесь завершения первого окна.' }
  } catch [Threading.AbandonedMutexException] {
    # Ownership is acquired after a crashed installer; recover its journal below.
  }
}
$journalPath = Join-Path $stateRoot 'install-transaction.json'
if ($Mode -ne 'Validate' -and (Test-Path -LiteralPath $journalPath)) {
  Assert-PlainTree $stateRoot
  Assert-Marker $stateRoot
  $pending = Get-Content -LiteralPath $journalPath -Raw | ConvertFrom-Json
  if ($pending.application_id -ne $appId) { throw 'LES Light: журнал установки не принадлежит приложению.' }
  $pendingBackup = [IO.Path]::GetFullPath([string]$pending.backup)
  $pendingStaging = [IO.Path]::GetFullPath([string]$pending.staging)
  foreach ($entry in @(@($pendingBackup, 'LES Light.rollback-*'), @($pendingStaging, 'LES Light.staging-*'))) {
    if ((Split-Path -Parent $entry[0]) -ne (Split-Path -Parent $installRoot) -or (Split-Path -Leaf $entry[0]) -notlike $entry[1]) {
      throw 'LES Light: недопустимый путь в журнале восстановления.'
    }
    Assert-Parents $entry[0]
    Assert-PlainTree $entry[0]
    if (Test-Path -LiteralPath $entry[0]) { Assert-Marker $entry[0] }
  }
  if (Test-Path -LiteralPath $installRoot) { Assert-PlainTree $installRoot; Assert-Marker $installRoot; Assert-AppStopped $installRoot }
  if (Test-Path -LiteralPath $pendingBackup) {
    Assert-AppStopped $pendingBackup
    if (Test-Path -LiteralPath $installRoot) {
      # Both copies are complete, marked application trees. Roll back the
      # uncommitted activation before accepting another installer action.
      if (Test-Path -LiteralPath $pendingStaging) { throw 'LES Light: неоднозначное состояние восстановления. Сохранены обе версии приложения.' }
      Move-Item -LiteralPath $installRoot -Destination $pendingStaging
    }
    try { Move-Item -LiteralPath $pendingBackup -Destination $installRoot }
    catch {
      if (Test-Path -LiteralPath $pendingStaging) { Move-Item -LiteralPath $pendingStaging -Destination $installRoot }
      throw
    }
  }
  if (Test-Path -LiteralPath $pendingStaging) { Remove-Item -LiteralPath $pendingStaging -Recurse -Force }
  Remove-Item -LiteralPath $journalPath -Force
}
if ($Mode -eq 'Recover') { Write-Output 'Восстановление LES Light завершено.'; exit 0 }
if ($Mode -eq 'Rollback') {
  Assert-PlainTree $installRoot
  Assert-Marker $installRoot
  Assert-AppStopped $installRoot
  Assert-PlainTree $stateRoot
  Assert-Marker $stateRoot
  $identity = Get-Content -LiteralPath (Join-Path $stateRoot 'light-install.json') -Raw | ConvertFrom-Json
  if (-not $identity.rollback_directory) { throw 'LES Light: предыдущая версия для отката отсутствует.' }
  $backup = [IO.Path]::GetFullPath([string]$identity.rollback_directory)
  if ((Split-Path -Parent $backup) -ne (Split-Path -Parent $installRoot) -or
      (Split-Path -Leaf $backup) -notlike 'LES Light.rollback-*') {
    throw 'LES Light: путь отката не принадлежит приложению.'
  }
  Assert-Parents $backup
  Assert-PlainTree $backup
  Assert-Marker $backup
  $failed = $installRoot + '.staging-' + [Guid]::NewGuid().ToString('N')
  Move-Item -LiteralPath $installRoot -Destination $failed
  try { Move-Item -LiteralPath $backup -Destination $installRoot }
  catch { Move-Item -LiteralPath $failed -Destination $installRoot; throw }
  Assert-PlainTree $failed
  Remove-Item -LiteralPath $failed -Recurse -Force
  @{ application_id=$appId } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stateRoot 'light-install.json') -Encoding UTF8
  Write-Output 'Предыдущая версия LES Light восстановлена. Документы и почта сохранены.'
  exit 0
}
if ($Mode -eq 'Remove') {
  $applicationTargets = @($installRoot)
  $programs = Split-Path -Parent $installRoot
  if (Test-Path -LiteralPath $programs) {
    $applicationTargets += @(Get-ChildItem -LiteralPath $programs -Directory -Force | Where-Object { $_.Name -like 'LES Light.rollback-*' } | ForEach-Object { $_.FullName })
  }
  # Validate both targets before removing either; full LES paths are never used.
  foreach ($target in $applicationTargets + $(if ($RemoveUserData) { @($stateRoot) } else { @() })) {
    if (Test-Path -LiteralPath $target) { Assert-PlainTree $target; Assert-Marker $target; Assert-AppStopped $target }
  }
  foreach ($target in $applicationTargets) { if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Recurse -Force } }
  if ($RemoveUserData -and (Test-Path -LiteralPath $stateRoot)) { Remove-Item -LiteralPath $stateRoot -Recurse -Force }
  Write-Output $(if ($RemoveUserData) { 'LES Light и его данные удалены.' } else { 'LES Light удалён. Документы, почта и настройки сохранены.' })
  exit 0
}

if (-not $Source) { throw 'LES Light: отсутствует пакет установки.' }
$sourceRoot = [IO.Path]::GetFullPath($Source)
Assert-Parents $sourceRoot
Assert-PlainTree $sourceRoot
$manifest = Get-Content -LiteralPath (Join-Path $sourceRoot 'light-package.json') -Raw | ConvertFrom-Json
if ($manifest.application_id -ne $appId -or $manifest.schema -ne 'les.light-package.v1') {
  throw 'LES Light: это пакет другого приложения. Установка отменена.'
}
$files = @($manifest.files)
if (-not ($files | Where-Object { $_.path -eq 'les-light.exe' })) {
  throw 'LES Light: пакет не содержит приложение. Загрузите полный установщик.'
}
$seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
Write-Output 'Проверяю целостность установочного комплекта...'
$checkedCount = 0
foreach ($file in $files) {
  $relative = [string]$file.path
  if (($relative -replace '\\','/') -match '(^|/)\.env$|^(data|storage|logs|RAG_Content|artifacts)/|^runtime/(data|storage|logs|RAG_Content|artifacts)/') {
    throw 'LES Light: пакет содержит личные данные или настройки. Установка отменена.'
  }
  if (-not $relative -or [IO.Path]::IsPathRooted($relative) -or $relative.Contains(':') -or
      ($relative -split '[/\\]' | Where-Object { $_ -in @('..', '.', '') }) -or -not $seen.Add($relative.Replace('\','/'))) {
    throw 'LES Light: пакет содержит недопустимый путь или дубликат. Установка отменена.'
  }
  $item = Join-Path $sourceRoot $relative
  if (-not (Test-Path -LiteralPath $item -PathType Leaf) -or
      (Get-PackageHash $item) -ne $file.sha256) {
    throw "LES Light: файл пакета повреждён: $relative. Повторите загрузку установщика."
  }
  $checkedCount++
  if (($checkedCount % 500) -eq 0) { Write-Output "Проверено файлов: $checkedCount из $($files.Count)" }
}
foreach ($item in Get-ChildItem -LiteralPath $sourceRoot -File -Recurse -Force) {
  $relative = $item.FullName.Substring($sourceRoot.TrimEnd('\').Length + 1).Replace('\','/')
  if ($relative -ne 'light-package.json' -and -not $seen.Contains($relative)) {
    throw "LES Light: файл отсутствует в манифесте: $relative. Установка отменена."
  }
}
if ($Mode -eq 'Validate') { Write-Output 'Пакет LES Light проверен.'; exit 0 }

$requiredBytes = 64MB
foreach ($file in $files) { $requiredBytes += (Get-Item -LiteralPath (Join-Path $sourceRoot $file.path)).Length }
if ((Get-FreeBytes $installRoot) -lt $requiredBytes) {
  throw 'LES Light: недостаточно места для новой версии. Освободите место на диске и повторите установку. Текущая версия сохранена.'
}

if (Test-Path -LiteralPath $installRoot) { Assert-PlainTree $installRoot; Assert-Marker $installRoot; Assert-AppStopped $installRoot }
if (Test-Path -LiteralPath $stateRoot) { Assert-PlainTree $stateRoot; Assert-Marker $stateRoot }
$staging = $installRoot + '.staging-' + [Guid]::NewGuid().ToString('N')
$backup = $installRoot + '.rollback-' + [Guid]::NewGuid().ToString('N')
New-Item -ItemType Directory -Path $staging -Force | Out-Null
$swapped = $false
try {
  Write-Output 'Копирую приложение. Документы пользователя сохраняются...'
  $copiedCount = 0
  foreach ($file in $files) {
    $destination = Join-Path $staging $file.path
    New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $sourceRoot $file.path) -Destination $destination
    if ((Get-PackageHash $destination) -ne $file.sha256) { throw 'LES Light: ошибка копирования пакета.' }
    $copiedCount++
    if (($copiedCount % 500) -eq 0) { Write-Output "Скопировано файлов: $copiedCount из $($files.Count)" }
  }
  @{ application_id=$appId; version=$manifest.version } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $staging 'light-install.json') -Encoding UTF8
  if (-not (Test-Path -LiteralPath $stateRoot)) {
    New-Item -ItemType Directory -Path $stateRoot -Force | Out-Null
    @{ application_id=$appId } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stateRoot 'light-install.json') -Encoding UTF8
  }
  $journalTemporary = $journalPath + '.tmp'
  @{ application_id=$appId; staging=$staging; backup=$backup } | ConvertTo-Json | Set-Content -LiteralPath $journalTemporary -Encoding UTF8
  Move-Item -LiteralPath $journalTemporary -Destination $journalPath -Force
  if (Test-Path -LiteralPath $installRoot) { Move-Item -LiteralPath $installRoot -Destination $backup }
  try { Move-Item -LiteralPath $staging -Destination $installRoot; $swapped = $true }
  catch {
    if (Test-Path -LiteralPath $backup) { Move-Item -LiteralPath $backup -Destination $installRoot }
    throw
  }
  New-Item -ItemType Directory -Path $stateRoot -Force | Out-Null
  @{ application_id=$appId; rollback_directory=$(if (Test-Path -LiteralPath $backup) { $backup } else { '' }) } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stateRoot 'light-install.json') -Encoding UTF8
  Remove-Item -LiteralPath $journalPath -Force
  # Keep the previous application version for an explicit rollback.
  Write-Output 'LES Light установлен. Данные предыдущей установки сохранены.'
} finally {
  if (-not $swapped -and (Test-Path -LiteralPath $staging)) {
    Assert-PlainTree $staging
    Remove-Item -LiteralPath $staging -Recurse -Force
  }
}
