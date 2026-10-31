# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
Measures how long the machine takes to start a trivial program, while the Windows tests run.

.DESCRIPTION
Some test runs on the hosted Windows runner have a few minutes in which every test that starts a
program (git, mitmproxy, a browser) runs 10 to 30 times slower, while tests that start nothing
do not. This canary makes such a window visible in every run, red or green.

-Out <file>: runs until stopped; every 2 s it starts `cmd /c exit 0`, times it to its end and
appends one line `<UTC time> <seconds>` to the file.

-Summary <file>: prints the runner image, the Defender state and exclusions, how many samples
there were, the median and the slowest, and every sample over 1 s.

PowerShell 7 (pwsh); keep this file ASCII-only.
#>
param([string]$Out, [string]$Summary)

if ($Out) {
    while ($true) {
        $t = (Get-Date).ToUniversalTime()
        $sw = [Diagnostics.Stopwatch]::StartNew()
        Start-Process -FilePath cmd.exe -ArgumentList '/c', 'exit 0' -NoNewWindow -Wait
        Add-Content -Path $Out -Value ('{0:HH:mm:ss.f} {1:n3}' -f $t, $sw.Elapsed.TotalSeconds)
        Start-Sleep -Seconds 2
    }
}

if ($Summary) {
    Write-Output "runner image: $env:ImageOS $env:ImageVersion"
    try {
        $s = Get-MpComputerStatus
        Write-Output ('Defender: AMServiceEnabled={0} RealTimeProtectionEnabled={1} OnAccessProtectionEnabled={2} IoavProtectionEnabled={3} BehaviorMonitorEnabled={4} AMEngineVersion={5} AntivirusSignatureVersion={6}' -f $s.AMServiceEnabled, $s.RealTimeProtectionEnabled, $s.OnAccessProtectionEnabled, $s.IoavProtectionEnabled, $s.BehaviorMonitorEnabled, $s.AMEngineVersion, $s.AntivirusSignatureVersion)
        $p = Get-MpPreference
        Write-Output ('Defender preferences: DisableRealtimeMonitoring={0} ScanAvgCPULoadFactor={1} ExclusionPath={2} ExclusionProcess={3}' -f $p.DisableRealtimeMonitoring, $p.ScanAvgCPULoadFactor, ($p.ExclusionPath -join ';'), ($p.ExclusionProcess -join ';'))
    } catch {
        Write-Output "Defender state not readable: $_"
    }
    $rows = @(Get-Content -Path $Summary -ErrorAction SilentlyContinue | ForEach-Object {
        $f = $_ -split ' '
        [pscustomobject]@{ At = $f[0]; Seconds = [double]$f[1] }
    })
    if ($rows.Count -eq 0) {
        Write-Output 'process start canary: no samples'
        exit 0
    }
    $sorted = @($rows | Sort-Object Seconds)
    $median = $sorted[[int][math]::Floor($sorted.Count / 2)].Seconds
    $slow = @($rows | Where-Object { $_.Seconds -gt 1 })
    Write-Output ('process start canary: {0} samples, median {1:n3} s, slowest {2:n3} s, {3} over 1 s' -f $rows.Count, $median, $sorted[-1].Seconds, $slow.Count)
    $slow | ForEach-Object { '  slow start at {0}: {1:n3} s' -f $_.At, $_.Seconds }
}
