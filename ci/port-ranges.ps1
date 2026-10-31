# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
Prints the TCP port ranges Windows keeps back, and says when they cover the ports the tests pick.

.DESCRIPTION
The test kit's quiet_port() picks a free port in 20000..32000. Windows refuses a bind in an
excluded range (Hyper-V, WSL and Docker reserve blocks). The dynamic range (movable with `netsh int
ipv4 set dynamicport`) is where outgoing connections take their ports, so a test port inside it can
collide with them. Either way a run could fail on a port the test did not cause. This prints `netsh int ipv4 show excludedportrange protocol=tcp` and `netsh int ipv4 show
dynamicport tcp`, then one summary line: `QUIET PORT RANGE HIT` with the overlapping ranges, or
`quiet port range clear`.

-SelfTest: checks the parsing and the overlap rule on fixed netsh output and exits 1 on a miss.

PowerShell 7 (pwsh); keep this file ASCII-only.
#>
param([switch]$SelfTest)

$QuietLow = 20000
$QuietHigh = 32000

# Returns the (Start, End) ranges that overlap QuietLow..QuietHigh, from the text of the two netsh
# commands. The excluded list has one `start end` line per range; the dynamic output has `Start
# Port` and `Number of Ports` lines.
function Get-QuietPortHits([string]$Excluded, [string]$Dynamic) {
    $ranges = @()
    foreach ($m in [regex]::Matches($Excluded, '(?m)^\s*(\d+)\s+(\d+)')) {
        $ranges += , @([int]$m.Groups[1].Value, [int]$m.Groups[2].Value, 'excluded')
    }
    $start = [regex]::Match($Dynamic, '(?i)Start Port\s*:\s*(\d+)')
    $count = [regex]::Match($Dynamic, '(?i)Number of Ports\s*:\s*(\d+)')
    if ($start.Success -and $count.Success) {
        $first = [int]$start.Groups[1].Value
        $last = $first + [int]$count.Groups[1].Value - 1
        $ranges += , @($first, $last, 'dynamic')
    }
    @($ranges | Where-Object { $_[0] -le $QuietHigh -and $_[1] -ge $QuietLow } |
        ForEach-Object { '{0} {1}-{2}' -f $_[2], $_[0], $_[1] })
}

if ($SelfTest) {
    $excluded = @'

Protocol tcp Port Exclusion Ranges

Start Port    End Port
----------    --------
      5357        5357     *
     49152       49251
     50000       50059     *

* - Administered port exclusions.
'@
    $dynamic = @'

Protocol tcp Dynamic Port Range
---------------------------------
Start Port      : 49152
Number of Ports : 16384
'@
    $failures = @()
    if ((Get-QuietPortHits $excluded $dynamic).Count -ne 0) { $failures += 'clear ranges reported a hit' }
    $hit = Get-QuietPortHits ($excluded + "`n     31950       32049`n") $dynamic
    if (($hit -join ';') -ne 'excluded 31950-32049') { $failures += "range crossing 32000 gave: $hit" }
    $moved = "Start Port      : 10000`nNumber of Ports : 15000`n"
    if ((@(Get-QuietPortHits $excluded $moved) -join ';') -ne 'dynamic 10000-24999') { $failures += 'moved dynamic range not reported' }
    if ((Get-QuietPortHits "     32001       32100`n" '').Count -ne 0) { $failures += 'range just above 32000 reported' }
    if ($failures) { $failures | ForEach-Object { Write-Output "self-test FAILED: $_" }; exit 1 }
    Write-Output 'port-ranges self-test passed'
    exit 0
}

$excludedText = (netsh int ipv4 show excludedportrange protocol=tcp) -join "`n"
$dynamicText = (netsh int ipv4 show dynamicport tcp) -join "`n"
Write-Output $excludedText
Write-Output $dynamicText
if ($excludedText -notmatch '\d+\s+\d+' -or $dynamicText -notmatch '(?i)Start Port\s*:\s*\d+') {
    Write-Output 'PORT RANGES NOT PARSED: read the netsh output above by hand'
    exit 0
}
$hits = @(Get-QuietPortHits $excludedText $dynamicText)
if ($hits.Count -gt 0) {
    Write-Output ('QUIET PORT RANGE HIT: {0} overlap {1}..{2}' -f ($hits -join ', '), $QuietLow, $QuietHigh)
} else {
    Write-Output ('quiet port range clear: no excluded or dynamic TCP range overlaps {0}..{1}' -f $QuietLow, $QuietHigh)
}
