# SPDX-License-Identifier: AGPL-3.0-or-later
<#
.SYNOPSIS
Prints what the Windows runner knows about its volumes, for a build that failed with "A device which
does not exist was specified" (os error 433).

.DESCRIPTION
That error on a file under D:\a\... can mean the runner VM's D: volume went away mid-build, which
neither a full disk (os error 112) nor our own build output explains. This prints, in sections that
never stop each other: Get-Volume, free space per drive (Get-PSDrive), Get-Disk, and the Ntfs, volmgr
and disk entries of the System event log from the last -Minutes minutes. It only reads; run it from
a step with `if: failure()` and `continue-on-error: true`.

-SelfTest: checks the event line formatting on a fixed record and exits 1 on a miss.

PowerShell 7 (pwsh); keep this file ASCII-only.
#>
param([int]$Minutes = 60, [int]$MaxEvents = 40, [switch]$SelfTest)

# One line per event: time, provider, id, level, then the message on one line, cut at 300 characters.
function Format-VolumeEvent($Event) {
    $text = ($Event.Message -replace '\s+', ' ')
    if ($text.Length -gt 300) { $text = $text.Substring(0, 300) }
    '{0:s} {1} {2} {3} {4}' -f $Event.TimeCreated, $Event.ProviderName, $Event.Id, $Event.LevelDisplayName, $text
}

if ($SelfTest) {
    $event = [pscustomobject]@{
        TimeCreated = [datetime]'2026-01-02T03:04:05'
        ProviderName = 'Ntfs'
        Id = 55
        LevelDisplayName = 'Error'
        Message = "line one`r`n  line two"
    }
    $got = Format-VolumeEvent $event
    $want = '2026-01-02T03:04:05 Ntfs 55 Error line one line two'
    if ($got -ne $want) { Write-Output "self-test failed: got '$got', want '$want'"; exit 1 }
    Write-Output 'self-test ok'
    exit 0
}

# Runs one section; a failing command prints its error and the next section still runs.
function Show-Section([string]$Title, [scriptblock]$Body) {
    Write-Output "--- $Title ---"
    try { & $Body } catch { Write-Output "(failed: $($_.Exception.Message))" }
}

Show-Section 'Get-Volume' {
    Get-Volume | Select-Object DriveLetter, FileSystemLabel, FileSystem, DriveType, HealthStatus, OperationalStatus, SizeRemaining, Size |
        Format-Table -AutoSize | Out-String -Width 200
}
Show-Section 'free space per drive (GB)' {
    Get-PSDrive -PSProvider FileSystem | ForEach-Object {
        '{0}: used {1:N1} free {2:N1}' -f $_.Name, ($_.Used / 1GB), ($_.Free / 1GB)
    }
}
Show-Section 'Get-Disk' {
    Get-Disk | Select-Object Number, FriendlyName, OperationalStatus, HealthStatus, IsOffline, PartitionStyle, Size |
        Format-Table -AutoSize | Out-String -Width 200
}
$since = (Get-Date).AddMinutes(-$Minutes)
foreach ($provider in 'Ntfs', 'volmgr', 'disk') {
    Show-Section "System event log, provider $provider, last $Minutes minutes (newest first, at most $MaxEvents)" {
        $found = @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; ProviderName = $provider; StartTime = $since } -MaxEvents $MaxEvents -ErrorAction SilentlyContinue)
        if ($found.Count -eq 0) { Write-Output '(none)' }
        $found | ForEach-Object { Format-VolumeEvent $_ }
    }
}
exit 0
