# SPDX-License-Identifier: AGPL-3.0-or-later
# The Windows Application event log entries of Puddle's processes since a time, for the Windows soak
# (soak-windows.mjs): every entry whose message names puddle-app, puddle.exe or msb.exe, written to -Out.
# Exit 1 when one of them is a crash or a hang (the provider is Application Error, Application Hang or
# Windows Error Reporting); 0 otherwise. -Plant first writes such an entry for puddle-app.exe, which is how
# the soak workflow shows the collector sees a crash. Windows PowerShell 5.1, ASCII only.
param(
  [Parameter(Mandatory = $true)][string]$Since,
  [Parameter(Mandatory = $true)][string]$Out,
  [switch]$Plant
)
$ErrorActionPreference = 'Stop'
$start = [datetime]::Parse($Since, [Globalization.CultureInfo]::InvariantCulture).ToUniversalTime()
if ($Plant) {
  Write-EventLog -LogName Application -Source 'Application Error' -EventId 1000 -EntryType Error `
    -Message 'Faulting application name: puddle-app.exe, version: 0.0.0.0 (planted by the soak)'
}
$crashProviders = @('Application Error', 'Application Hang', 'Windows Error Reporting')
$names = 'puddle-app|puddle\.exe|msb\.exe'
$found = @()
try {
  $found = @(Get-WinEvent -FilterHashtable @{ LogName = 'Application'; StartTime = $start } -ErrorAction Stop |
      Where-Object { $_.Message -match $names })
} catch {
  # Get-WinEvent says so when nothing matches the filter; any other failure is a real one.
  if ($_.FullyQualifiedErrorId -notlike 'NoMatchingEventsFound*') { throw }
}
$lines = $found | ForEach-Object { '{0:o} {1} id={2} {3}' -f $_.TimeCreated.ToUniversalTime(), $_.ProviderName, $_.Id, (($_.Message -split "`r?`n")[0]) }
Set-Content -LiteralPath $Out -Value @($lines) -Encoding ascii
$crashes = @($found | Where-Object { $crashProviders -contains $_.ProviderName })
"events of Puddle's processes since ${Since}: $($found.Count), crashes or hangs: $($crashes.Count)"
if ($crashes.Count -gt 0) { exit 1 }
