# Compare hash reads on a quiet synthetic Windows folder, then deliberately drop an event.
param([Parameter(Mandatory)][string]$Scratch, [Parameter(Mandatory)][string]$Output)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path -LiteralPath $Scratch).Path
$folder = Join-Path $root ('watch-trial-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $folder
$watcher = $null
try {
    $block = [byte[]]::new(1MB)
    $known = @{}
    for ($i=0; $i -lt 64; $i++) {
        $block[0] = $i
        $file = Join-Path $folder ("document-{0:D2}.bin" -f $i)
        [IO.File]::WriteAllBytes($file,$block)
        $known[$file] = (Get-FileHash -LiteralPath $file).Hash
    }
    $watcher = [IO.FileSystemWatcher]::new($folder)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    foreach ($file in $known.Keys) {
        if ((Get-FileHash -LiteralPath $file).Hash -ne $known[$file]) { throw 'Unexpected change' }
    }
    $quietMillis = $clock.ElapsedMilliseconds
    $watcher.EnableRaisingEvents = $true
    $eventId = 'trial-' + [Guid]::NewGuid().ToString('N')
    $null = Register-ObjectEvent -InputObject $watcher -EventName Changed -SourceIdentifier $eventId
    $file = Join-Path $folder 'document-00.bin'
    $stamp = [IO.File]::GetLastWriteTimeUtc($file)
    $clock.Restart()
    $block[0] = 81
    [IO.File]::WriteAllBytes($file,$block)
    [IO.File]::SetLastWriteTimeUtc($file,$stamp)
    $event = Wait-Event -SourceIdentifier $eventId -Timeout 5
    if (!$event -or $event.SourceEventArgs.FullPath -ne $file) { throw 'Missing file event' }
    $hash = (Get-FileHash -LiteralPath $file).Hash
    if ($hash -eq $known[$file]) { throw 'Undetected content change' }
    $known[$file] = $hash
    $observedMillis = $clock.ElapsedMilliseconds
    $watcher.EnableRaisingEvents = $false
    $block[0] = 82
    [IO.File]::WriteAllBytes($file,$block)
    [IO.File]::SetLastWriteTimeUtc($file,$stamp)
    $found = @($known.Keys | Where-Object { (Get-FileHash -LiteralPath $_).Hash -ne $known[$_] })
    if ($found.Count -ne 1 -or $found[0] -ne $file) { throw 'Reconciliation missed the lost event' }
    [ordered]@{files=64; file_bytes=$block.Length; quiet_full_hash_bytes=64*$block.Length
        quiet_full_hash_ms=$quietMillis; quiet_notification_hash_bytes=0
        changed_file_hash_bytes=$block.Length; event_and_hash_ms=$observedMillis
        missed_event_reconciled=$true; timestamp_preserved=([IO.File]::GetLastWriteTimeUtc($file) -eq $stamp)
    } | ConvertTo-Json | Set-Content -LiteralPath $Output -Encoding utf8
} finally {
    if ($eventId) { Unregister-Event -SourceIdentifier $eventId -ErrorAction SilentlyContinue }
    if ($watcher) { $watcher.Dispose() }
    # Remove only the generated files from this trial. Leave unexpected entries untouched.
    if ((Split-Path -Parent (Resolve-Path -LiteralPath $folder).Path) -eq $root) {
        foreach ($file in $known.Keys) { [IO.File]::Delete($file) }
        [IO.Directory]::Delete($folder, $false)
    }
}
