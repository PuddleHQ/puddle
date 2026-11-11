# SPDX-License-Identifier: GPL-3.0-or-later
# Sandbox controls on this runner: WHP probe matrix, msb CLI-level boots, IFEO mitigations on msb.exe.
# Usage: probe.ps1 <tools dir> <msb dir> <out dir>
param([string]$Tools, [string]$Msb, [string]$Out)
$ErrorActionPreference = 'Continue'
New-Item -ItemType Directory -Force $Out | Out-Null
$log = "$Out\probe.txt"
function Say($s) { $s | Tee-Object -FilePath $log -Append | Out-Host }
$os = Get-CimInstance Win32_OperatingSystem; $cs = Get-CimInstance Win32_ComputerSystem
Say "OS: $($os.Caption) $($os.Version) build $($os.BuildNumber); CPU: $((Get-CimInstance Win32_Processor).Name); HypervisorPresent=$($cs.HypervisorPresent); domain=$($cs.PartOfDomain)"
Say ((whoami /groups | Select-String 'Mandatory Label|Administrators|Hyper-V') -join "`n")
Say (& "$Tools\sd-dump.exe" '\\?\VidExo')
$L = "$Tools\sbx-launch.exe"; $P = "$Tools\whp-probe.exe"
$probeSpecs = @(
  @('--token','full'), @('--token','restricted'), @('--token','rsids'), @('--token','low'), @('--token','untrusted'),
  @('--ac','puddle-trial'), @('--ac','puddle-trial','--cap','hypervisorPlatform'),
  @('--ac','puddle-trial','--lpac','--cap','hypervisorPlatform'),
  @('--ac','puddle-trial','--cap','hypervisorPlatform','--token','lowbox','--swap'),
  @('--token','low','--swap'), @('--token','restricted','--swap'))
foreach ($m in 'acg','msonly','win32k','extpoint','cfg','strict-cfg','font','no-remote-image','no-low-label-image','prefer-system32','strict-handle','heap-terminate','force-aslr','cet','cet-strict','block-non-cet','fsctl','loader-integrity','module-tamper') { $probeSpecs += ,@('--mit', $m) }
$probeSpecs += ,@('--child-block'); $probeSpecs += ,@('--job','ui','--job','active1')
icacls $Tools /grant '*S-1-15-2-1:(OI)(CI)RX' | Out-Null
Say "## WHP probe (256 MiB) under each control"
foreach ($s in $probeSpecs) {
  $o = & $L @s -- $P 256 2>&1 | Out-String
  $res = ($o -split "`n" | Where-Object { $_ -match 'token:|FAIL|RESULT' }) -join ' | '
  Say ("probe [{0}] {1}" -f ($s -join ' '), $res.Trim())
}
# CLI-level: the msb CLI and the runtime it starts both run under the control (tokens inherit).
$homeBase = "$env:RUNNER_TEMP\t-home"; $homeLow = "$env:RUNNER_TEMP\t-home-low"; $homeAc = "$env:RUNNER_TEMP\t-home-ac"
New-Item -ItemType Directory -Force $homeBase, "$homeLow\tmp", "$homeAc\tmp" | Out-Null
icacls $homeLow /setintegritylevel '(OI)(CI)low' | Out-Null
icacls $homeAc /grant '*S-1-15-2-1:(OI)(CI)F' | Out-Null
icacls $Msb /grant '*S-1-15-2-1:(OI)(CI)RX' | Out-Null
$msbExe = "$Msb\msb.exe"
function Boot($label, $homeDir, $tmp, [string[]]$spec, [int]$n) {
  $env:MSB_HOME = $homeDir
  $oldTemp = $env:TEMP; $oldTmp = $env:TMP
  if ($tmp) { $env:TEMP = $tmp; $env:TMP = $tmp }
  $times = @()
  for ($i = 0; $i -lt $n; $i++) {
    $o = & $L @spec -- $msbExe run alpine:3.20 --memory 512M -- sh -c 'echo GUEST-OK' 2>&1 | Out-String
    $ok = $o -match 'GUEST-OK'
    if ($ok -and $o -match 'ms=(\d+)') { $times += [int]$Matches[1] }
    if (-not $ok -or $i -eq 0) { Say ("boot [{0}] run {1}: {2} :: {3}" -f $label, $i, $(if ($ok) {'OK'} else {'FAIL'}), (($o -split "`n" | Where-Object { $_ -match 'error|LAUNCH|denied|other' }) -join ' | ').Trim()) }
    if (-not $ok) { break }
  }
  if ($times.Count -gt 1) {
    $t = @($times | Select-Object -Skip 1 | Sort-Object)
    Say ("boot [{0}] n={1} median_ms={2} min_ms={3} max_ms={4}" -f $label, $t.Count, $t[[int]($t.Count/2)], $t[0], $t[-1])
  }
  $env:TEMP = $oldTemp; $env:TMP = $oldTmp
}
function ClearIfeo { Remove-Item -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\msb.exe' -Recurse -Force -ErrorAction SilentlyContinue }
Say "## msb run alpine (CLI + runtime under the control); first run pulls the image"
Boot 'base' $homeBase $null @('--token','full') 11
Boot 'restricted' $homeBase $null @('--token','restricted') 11
Boot 'rsids' $homeBase $null @('--token','rsids') 11
# An administrator's folders grant Administrators, which these tokens hold deny-only: grant the user SID.
$homeUser = "$env:RUNNER_TEMP\t-home-user"
New-Item -ItemType Directory -Force $homeUser | Out-Null
icacls $homeUser /grant "$($env:USERNAME):(OI)(CI)F" | Out-Null
Boot 'restricted (home granted to the user SID)' $homeUser $null @('--token','restricted') 11
Boot 'rsids (home granted to the user SID)' $homeUser $null @('--token','rsids') 11
Boot 'low (home+temp low-labelled)' $homeLow "$homeLow\tmp" @('--token','low') 11
Boot 'low (home medium)' $homeBase $null @('--token','low') 1
Boot 'untrusted' $homeLow "$homeLow\tmp" @('--token','untrusted') 1
Boot 'appcontainer+hypervisorPlatform+internetClient' $homeAc "$homeAc\tmp" @('--ac','puddle-trial','--cap','hypervisorPlatform','--cap','internetClient') 1
Boot 'base again' $homeBase $null @('--token','full') 11
# IFEO: Windows applies these to every msb.exe at creation, so the runtime gets them too.
$sets = [ordered]@{
  'acg' = @('BlockDynamicCode'); 'child' = @('DisallowChildProcessCreation'); 'msonly' = @('MicrosoftSignedOnly');
  'storesigned' = @('MicrosoftSignedOnly','AllowStoreSignedBinaries'); 'win32k' = @('DisableWin32kSystemCalls');
  'extpoint' = @('DisableExtensionPoints'); 'imageload' = @('BlockRemoteImageLoads','BlockLowLabelImageLoads','PreferSystem32');
  'font' = @('DisableNonSystemFonts'); 'strictcfg' = @('CFG','StrictCFG'); 'cetstrict' = @('UserShadowStack','UserShadowStackStrictMode');
  'cfg' = @('CFG'); 'strict-handle' = @('StrictHandle'); 'aslr' = @('ForceRelocateImages','BottomUp','HighEntropy'); 'sehop-heap' = @('SEHOP','TerminateOnError');
  'strong-no-child' = @('BlockDynamicCode','DisableExtensionPoints','DisableNonSystemFonts','BlockRemoteImageLoads','BlockLowLabelImageLoads','PreferSystem32','StrictHandle','ForceRelocateImages','BottomUp','HighEntropy','SEHOP','TerminateOnError','UserShadowStack','UserShadowStackStrictMode')
}
foreach ($k in $sets.Keys) {
  ClearIfeo
  Set-ProcessMitigation -Name msb.exe -Enable $sets[$k]
  Say ("ifeo set {0}: {1}" -f $k, ((Get-ProcessMitigation -Name msb.exe | Out-String -Width 300) -split "`n" | Where-Object { $_ -match ':\s+ON' } | ForEach-Object { $_.Trim() }) -join ', ')
  Boot "ifeo $k" $homeBase $null @('--token','full') $(if ($k -eq 'strong-no-child') { 11 } else { 1 })
}
ClearIfeo
Set-ProcessMitigation -Name msb.exe -Enable $sets['strong-no-child']
Boot 'ifeo strong-no-child + restricted token' $homeUser $null @('--token','restricted') 11
Boot 'ifeo strong-no-child + low token' $homeLow "$homeLow\tmp" @('--token','low') 11
ClearIfeo
Boot 'base after ifeo' $homeBase $null @('--token','full') 11
Say "## code integrity / mitigation events naming msb"
Get-WinEvent -LogName 'Microsoft-Windows-Security-Mitigations/KernelMode','Microsoft-Windows-Security-Mitigations/UserMode','Microsoft-Windows-CodeIntegrity/Operational' -MaxEvents 200 -ErrorAction SilentlyContinue |
  Where-Object { $_.Message -match 'msb' } | Select-Object -First 15 | ForEach-Object { $m = $_.Message -replace "`r?`n", ' '; Say ("event {0} {1}: {2}" -f $_.Id, $_.ProviderName, $m.Substring(0, [Math]::Min(300, $m.Length))) }
