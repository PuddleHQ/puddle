# SPDX-License-Identifier: GPL-3.0-or-later
# Installs Puddle's MSI on a clean Windows machine the way a user would (per user, silent), starts
# the installed program, runs the system check against the installed runtime, upgrades it with a
# newer MSI, and uninstalls it, checking at each step what the installer promises:
#   - the install is per user (the MSI says no elevation is needed; the files go under
#     %LOCALAPPDATA%\Programs\Puddle; nothing under Program Files; one Uninstall entry);
#   - the program, the CLI, the runtime folder (msb, firmware, guest agent, code-server) and the
#     licences are where the program looks for them;
#   - the installed program starts, answers its API, and its system check finds the runtime; the
#     CLI's `doctor --json` agrees;
#   - an upgrade over the older MSI keeps what the first run wrote in the data folder;
#   - the "Start Puddle when I sign in" switch writes the sign-in entry (the current user's Run key)
#     for the installed program, and the switch off removes it; the entry survives an upgrade, and the uninstall removes it, so no sign-in starts a program that is gone;
#   - the installed program boots a real virtual machine: the system check's test boot passes within
#     30 s, and a workspace made over the API from the stock image runs a command, stops and is
#     deleted (create to running within 180 s, the first image pull included), leaving no runtime
#     process behind; where the machine has no usable hypervisor this is a named skip, and a
#     failure on a pull request (the policy of the vm-windows workflow);
#   - uninstalling removes the install folder, the Start menu entry and the registry keys, and
#     leaves no file outside Puddle's own data folders (listed below).
# Usage: pwsh ci/msi-install-test.ps1 -Msi <first.msi> [-UpgradeMsi <newer.msi>] [-LogDir <dir>]
#        pwsh ci/msi-install-test.ps1 -SelfTest      (the helpers' own checks; needs no MSI)
# Without -UpgradeMsi the upgrade step is skipped and the first MSI is the one uninstalled.
# Where the machine has no usable hypervisor, a failing `virtualization` or `hypervisor` check is
# reported but does not fail the test (it says nothing about the installer), the boot steps are
# skipped and say so; every other failing check fails.
[CmdletBinding(DefaultParameterSetName = 'Run')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Run')][string]$Msi,
    [Parameter(ParameterSetName = 'Run')][string]$UpgradeMsi = '',
    [Parameter(ParameterSetName = 'Run')][string]$LogDir = $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }),
    [Parameter(Mandatory = $true, ParameterSetName = 'SelfTest')][switch]$SelfTest
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$ProductName = 'Puddle'
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\$ProductName"
# Puddle's own data (the store, the API connection file, msb's home) and the shell's per-user
# state under its application identifier (the WebView2 profile, the window position). The
# uninstaller leaves these on purpose: they are the user's data and settings.
$DataDir = Join-Path $env:LOCALAPPDATA 'puddle'
$AllowedLeftovers = @('puddle', 'be.tijsvankampen.puddle')

function Step([string]$Message) { Write-Host "==> $Message" }
function Fail([string]$Message) { throw "msi-install-test: $Message" }
function Expect([bool]$Condition, [string]$Message) { if (-not $Condition) { Fail $Message } }

# Top-level names of the folders an installer could drop something into, plus the full listing of
# the per-user programs and Start menu folders, plus the Puddle registry keys.
function Get-Footprint {
    $lines = New-Object System.Collections.Generic.List[string]
    $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
    $shallow = @($env:LOCALAPPDATA, $env:APPDATA, $env:USERPROFILE, $env:ProgramFiles, ${env:ProgramFiles(x86)},
        $env:ProgramData, [Environment]::GetFolderPath('Desktop'))
    foreach ($root in $shallow | Where-Object { $_ -and (Test-Path -LiteralPath $_) }) {
        Get-ChildItem -LiteralPath $root -Force -ErrorAction SilentlyContinue |
            ForEach-Object { $lines.Add("top $($_.FullName)") }
    }
    foreach ($root in @((Join-Path $env:LOCALAPPDATA 'Programs'), $startMenu)) {
        if (Test-Path -LiteralPath $root) {
            Get-ChildItem -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue |
                ForEach-Object { $lines.Add("deep $($_.FullName)") }
        }
    }
    foreach ($key in @('HKCU:\Software\Puddle', 'HKLM:\Software\Puddle')) {
        if (Test-Path -LiteralPath $key) { $lines.Add("reg $key") }
    }
    $lines | Sort-Object -Unique
}

function Get-UninstallEntries {
    $views = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')
    foreach ($view in $views) {
        if (-not (Test-Path -LiteralPath $view)) { continue }
        Get-ChildItem -LiteralPath $view | ForEach-Object {
            $p = Get-ItemProperty -LiteralPath $_.PSPath
            if ($p.PSObject.Properties['DisplayName'] -and $p.DisplayName -eq $ProductName) {
                [pscustomobject]@{ Hive = $view.Substring(0, 4); Version = $p.DisplayVersion; Key = $_.PSPath }
            }
        }
    }
}

# Is the MSI marked "no elevation needed" (summary information word count, bit 8) and does it
# set no ALLUSERS value (which would make it per machine)?
function Test-PerUserPackage([string]$Path) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $Path).Path, 0))
    $summary = $db.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $db, @(0))
    $words = $summary.GetType().InvokeMember('Property', 'GetProperty', $null, $summary, @(15))
    $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @("SELECT Value FROM Property WHERE Property='ALLUSERS'"))
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    $row = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
    $allUsers = $null -ne $row
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    return ((($words -band 8) -eq 8) -and -not $allUsers)
}

function Invoke-Msiexec([string[]]$Arguments, [string]$LogName) {
    $log = Join-Path $LogDir $LogName
    $all = $Arguments + @('/qn', '/norestart', '/l*v', "`"$log`"")
    Step "msiexec $($Arguments -join ' ')  (log: $log)"
    $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList $all -Wait -PassThru
    Expect ($p.ExitCode -eq 0) "msiexec $($Arguments -join ' ') exited with $($p.ExitCode); see $log"
}

function Stop-Installed {
    Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.Id -Force; $_.WaitForExit(30000) | Out-Null }
}

# Starts the installed program, waits for its connection file, and returns the API's connection.
function Start-Installed {
    $connectionFile = Join-Path $DataDir 'api.json'
    Remove-Item -LiteralPath $connectionFile -Force -ErrorAction SilentlyContinue
    $app = Start-Process -FilePath (Join-Path $InstallDir 'puddle-app.exe') -PassThru
    $deadline = (Get-Date).AddSeconds(120)
    while (-not (Test-Path -LiteralPath $connectionFile)) {
        if ($app.HasExited) { Fail "puddle-app exited with $($app.ExitCode) before it wrote $connectionFile" }
        if ((Get-Date) -gt $deadline) { Fail "puddle-app wrote no $connectionFile in 120 s" }
        Start-Sleep -Milliseconds 500
    }
    Start-Sleep -Seconds 5
    Expect (-not $app.HasExited) "puddle-app exited with $($app.ExitCode) within 5 s of starting"
    Get-Content -LiteralPath $connectionFile -Raw | ConvertFrom-Json
}

# Puddle's start-at-sign-in entry: the value Windows starts programs from at sign-in.
$RunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$RunValue = $ProductName

function Get-LoginEntry {
    $item = Get-ItemProperty -LiteralPath $RunKey -Name $RunValue -ErrorAction SilentlyContinue
    if ($item) { $item.$RunValue } else { $null }
}

# The command the program writes: its installed path in quotes, then the flag that opens it hidden.
function Get-ExpectedLoginEntry { "`"$(Join-Path $InstallDir 'puddle-app.exe')`" --at-login" }

# Turns the Settings switch on or off through the running program's API.
function Set-StartAtLogin($Connection, [bool]$On) {
    $headers = @{ Authorization = "Bearer $($Connection.token)" }
    $body = @{ ui = @{ start_at_login = $On } } | ConvertTo-Json -Depth 4
    Invoke-RestMethod -Method Put -Uri "$($Connection.url)/api/settings" -Headers $headers -ContentType 'application/json' -Body $body | Out-Null
}

# Waits for the entry to be $Expected ($null: gone). The program applies the switch within a few seconds.
function Wait-LoginEntry($Expected, [string]$What) {
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-LoginEntry) -ne $Expected) {
        if ((Get-Date) -gt $deadline) { Fail "$What; the sign-in entry is '$(Get-LoginEntry)', expected '$Expected'" }
        Start-Sleep -Milliseconds 500
    }
}

# The tolerated checks: what a hosted runner may lack, unrelated to the installer.
$RunnerLimits = @('virtualization', 'hypervisor')

# The bars: a test boot under 30 s; a workspace running within 180 s of the create, first pull included.
$BootBudgetSec = 30
$CreateBudgetSec = 180

# Polls $Probe about once a second until it returns something truthy, and returns that; fails
# naming $What when the bound runs out.
function Wait-Until([string]$What, [int]$TimeoutSec, [scriptblock]$Probe, [int]$PollMs = 1000) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ($true) {
        $value = & $Probe
        if ($value) { return $value }
        if ((Get-Date) -gt $deadline) { Fail "$What (gave up after $TimeoutSec s)" }
        Start-Sleep -Milliseconds $PollMs
    }
}

# Whether this machine may boot VMs, with the policy of the vm-windows workflow: no usable
# hypervisor is a skip, except on a pull request or in the merge queue, where a required result
# must not pass without running ('fail').
function Get-VmPolicy([bool]$HypervisorPresent, [string]$PlatformState, [string]$EventName) {
    if ($HypervisorPresent -and $PlatformState -eq 'Enabled') { return 'run' }
    if ($EventName -in @('pull_request', 'merge_group')) { return 'fail' }
    'skip'
}

function Get-VmFacts {
    $present = [bool](Get-CimInstance Win32_ComputerSystem).HypervisorPresent
    # Asking needs administrator rights; a machine that will not say is one that cannot be counted on.
    $platform = try { "$((Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -ErrorAction Stop).State)" } catch { 'Unknown' }
    [pscustomobject]@{ Present = $present; Platform = $platform }
}

# The seconds the test boot took, from a doctor report whose test boot passed.
function Get-TestBootSeconds($Report, [string]$Source) {
    $check = $Report.checks | Where-Object { $_.id -eq 'test_boot' }
    Expect ($null -ne $check) "${Source}: the report has no 'test_boot' check"
    Expect ($check.status -eq 'ok') "${Source}: the test boot is '$($check.status)': $($check.summary)"
    Expect ($check.summary -match 'in (\d+)\.(\d) s') "${Source}: the test boot's summary has no time: $($check.summary)"
    [double]::Parse("$($Matches[1]).$($Matches[2])", [cultureinfo]::InvariantCulture)
}

function Assert-DoctorGreen($Report, [string]$Source, [bool]$RequireBoot = $false) {
    Expect ($Report.schema_version -ge 1) "${Source}: no schema_version in the report"
    if ($RequireBoot) {
        $seconds = Get-TestBootSeconds $Report $Source
        Expect ($seconds -le $BootBudgetSec) "${Source}: the test boot took $seconds s, over the $BootBudgetSec s bar"
        Write-Host "${Source}: the test boot took $seconds s"
    }
    foreach ($id in @('runtime', 'launch')) {
        $check = $Report.checks | Where-Object { $_.id -eq $id }
        Expect ($null -ne $check) "${Source}: the report has no '$id' check"
        Expect ($check.status -eq 'ok') "${Source}: check '$id' is '$($check.status)': $($check.summary)"
    }
    foreach ($check in $Report.checks | Where-Object { $_.status -eq 'fail' }) {
        Expect ($RunnerLimits -contains $check.id) "${Source}: check '$($check.id)' failed: $($check.summary)"
        Write-Host "::notice::${Source}: '$($check.id)' fails on this machine ($($check.summary)); not an installer problem"
    }
    Write-Host "${Source}: ok=$($Report.ok), checks: $(($Report.checks | ForEach-Object { "$($_.id)=$($_.status)" }) -join ', ')"
}

function Assert-InstalledLayout {
    $required = @(
        'puddle-app.exe', 'puddle.exe',
        'runtime\msb.exe', 'runtime\libkrunfw.dll', 'runtime\runtime.json', 'runtime\puddle-agent',
        'runtime\code-server\bin\code-server', 'runtime\code-server\package.json',
        'runtime\licenses\NOTICE.txt', 'runtime\licenses\GPL-3.0.txt',
        'runtime\licenses\THIRD-PARTY-msb.txt', 'runtime\licenses\THIRD-PARTY-puddle.txt',
        'runtime\licenses\THIRD-PARTY-ui.txt', 'runtime\licenses\code-server\LICENSE')
    foreach ($name in $required) {
        Expect (Test-Path -LiteralPath (Join-Path $InstallDir $name) -PathType Leaf) "the install has no $name"
    }
    $entries = @(Get-UninstallEntries)
    Expect ($entries.Count -eq 1) "expected one '$ProductName' entry in Add or remove programs, found $($entries.Count)"
    # An elevated per-user install (a runner's) may register its entry in HKLM; where the files and
    # the package flag say per user, the hive is only reported.
    Write-Host "Add or remove programs entry: $($entries[0].Key)"
    Expect (-not (Test-Path -LiteralPath (Join-Path $env:ProgramFiles $ProductName))) "something was installed under Program Files"
    $entries[0]
}

# Puddle's REST API on the running program: $Conn is the connection file's content.
function Invoke-PuddleApi($Conn, [string]$Method, [string]$Path, $Body = $null, [int]$TimeoutSec = 60) {
    $request = @{
        Method = $Method; Uri = "$($Conn.url)$Path"; TimeoutSec = $TimeoutSec
        Headers = @{ Authorization = "Bearer $($Conn.token)" }
    }
    if ($null -ne $Body) { $request.ContentType = 'application/json'; $request.Body = ($Body | ConvertTo-Json -Depth 6) }
    Invoke-RestMethod @request
}

# $Policy is 'run' (boot VMs) or 'skip' (this machine cannot; the doctor's test boot is left out).
function Assert-InstalledWorks([string]$Policy) {
    $boot = $Policy -eq 'run'
    $conn = Start-Installed
    try {
        $query = if ($boot) { '' } else { '?boot=false' }
        $apiReport = Invoke-PuddleApi $conn 'Get' "/api/doctor$query" -TimeoutSec 120
        Assert-DoctorGreen $apiReport 'installed program, GET /api/doctor' $boot
    } finally {
        Stop-Installed
    }
    $flags = if ($boot) { @() } else { @('--no-boot') }
    $json = & (Join-Path $InstallDir 'puddle.exe') doctor --json @flags
    $cliReport = $json | ConvertFrom-Json
    Assert-DoctorGreen $cliReport "puddle doctor --json $flags" $boot
}

# Processes running from the install folder, other than the program itself: msb and the VMs it
# runs. A workspace that is gone must leave none.
function Get-RuntimeProcesses {
    @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) -and $_.Name -ne 'puddle-app'
    })
}

# Runs the installed msb with Puddle's private home, as the program itself does.
function Invoke-InstalledMsb([string[]]$Arguments) {
    $runtime = Join-Path $InstallDir 'runtime'
    $home_ = Join-Path $DataDir 'msb'
    $saved = @{}
    $set = @{ MSB_PATH = (Join-Path $runtime 'msb.exe'); MSB_HOME = $home_; MSB_CONFIG_PATH = (Join-Path $home_ 'config.json') }
    foreach ($name in $set.Keys) { $saved[$name] = [Environment]::GetEnvironmentVariable($name); [Environment]::SetEnvironmentVariable($name, $set[$name]) }
    try {
        $out = & (Join-Path $runtime 'msb.exe') @Arguments 2>&1
        [pscustomobject]@{ Code = $LASTEXITCODE; Output = (($out | ForEach-Object { "$_" }) -join "`n") }
    } finally {
        foreach ($name in $set.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
    }
}

function Get-WorkspaceNow($Conn, [string]$Name) {
    (Invoke-PuddleApi $Conn 'Get' '/api/workspaces').workspaces | Where-Object { $_.name -eq $Name }
}

# A workspace from the stock image, made, run in, stopped and deleted through the installed
# program's API, the way the app's own buttons do.
function Test-WorkspaceLifecycle($Conn) {
    $name = 'msi-test'
    Expect ($null -eq (Get-WorkspaceNow $Conn $name)) "a workspace called $name exists before the test"
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    Invoke-PuddleApi $Conn 'Post' '/api/workspaces' @{ name = $name } | Out-Null
    $started = $false
    $running = Wait-Until "workspace $name is not running" $CreateBudgetSec {
        $w = Get-WorkspaceNow $Conn $name
        if ($null -eq $w) { Fail "workspace $name vanished while it was being made" }
        if ($w.status -in @('crashed', 'volume_missing')) { Fail "workspace $name is $($w.status)" }
        if ($w.status -eq 'running' -and -not $w.busy) { return $w }
        # A create that ends with the workspace made but down is started once, as the app does.
        if (-not $started -and -not $w.busy -and $w.status -in @('created', 'stopped')) {
            $started = $true
            Invoke-PuddleApi $Conn 'Post' "/api/workspaces/$name/start" | Out-Null
        }
    }
    Write-Host "workspace $name running $([math]::Round($clock.Elapsed.TotalSeconds)) s after the create (image: $($running.image))"
    Expect ($clock.Elapsed.TotalSeconds -le $CreateBudgetSec) "the create took over $CreateBudgetSec s"

    $token = [guid]::NewGuid().ToString()
    $ran = Invoke-InstalledMsb @('exec', $name, '--', 'sh', '-c', "echo $token; . /etc/os-release; echo `$ID")
    Expect ($ran.Code -eq 0) "msb exec in $name exited with $($ran.Code): $($ran.Output)"
    Expect ($ran.Output -match [regex]::Escape($token)) "the command in $name did not run: $($ran.Output)"
    Expect ($ran.Output -match '(?m)^debian\s*$') "the workspace is not the stock debian image: $($ran.Output)"
    Write-Host "ran a command inside $name"

    Invoke-PuddleApi $Conn 'Post' "/api/workspaces/$name/stop" | Out-Null
    Wait-Until "workspace $name did not stop" 120 {
        $w = Get-WorkspaceNow $Conn $name
        $null -ne $w -and -not $w.busy -and $w.status -eq 'stopped'
    } | Out-Null
    Invoke-PuddleApi $Conn 'Delete' "/api/workspaces/$name" @{ confirm = $true } | Out-Null
    Wait-Until "workspace $name is still listed after the delete" 120 { $null -eq (Get-WorkspaceNow $Conn $name) } | Out-Null
    Wait-Until 'a runtime process is still running after the workspace was deleted' 60 {
        @(Get-RuntimeProcesses).Count -eq 0
    } | Out-Null
    $left = Invoke-InstalledMsb @('ls')
    Expect ($left.Output -notmatch [regex]::Escape($name)) "msb still lists $name after the delete: $($left.Output)"
    Write-Host "workspace $name stopped and deleted; no runtime process left"
}

if ($SelfTest) {
    $failures = 0
    function Check([string]$What, [bool]$Ok) {
        if ($Ok) { Write-Host "ok   $What" } else { Write-Host "FAIL $What"; $script:failures++ }
    }
    function Throws([scriptblock]$Block, [string]$Match) {
        try { & $Block; return $false } catch { return ($_.Exception.Message -match $Match) }
    }
    function Report([string]$BootStatus, [string]$BootSummary) {
        [pscustomobject]@{
            schema_version = 1
            checks = @(
                [pscustomobject]@{ id = 'runtime'; status = 'ok'; summary = 'found' },
                [pscustomobject]@{ id = 'launch'; status = 'ok'; summary = 'starts' },
                [pscustomobject]@{ id = 'test_boot'; status = $BootStatus; summary = $BootSummary })
        }
    }
    Check 'usable hypervisor runs everywhere' ((Get-VmPolicy $true 'Enabled' 'pull_request') -eq 'run')
    Check 'no hypervisor fails a pull request' ((Get-VmPolicy $false 'Disabled' 'pull_request') -eq 'fail')
    Check 'no hypervisor fails the merge queue' ((Get-VmPolicy $true 'Disabled' 'merge_group') -eq 'fail')
    Check 'no hypervisor skips a nightly run' ((Get-VmPolicy $false 'Disabled' 'schedule') -eq 'skip')
    Check 'no hypervisor skips a dispatched run' ((Get-VmPolicy $true 'Disabled' 'workflow_dispatch') -eq 'skip')
    Check 'boot seconds parsed' ((Get-TestBootSeconds (Report 'ok' 'a test VM booted and ran a program in 2.3 s') 't') -eq 2.3)
    Check 'a failed boot is named' (Throws { Get-TestBootSeconds (Report 'fail' 'msb crashed') 't' } "test boot is 'fail': msb crashed")
    Check 'a boot without a time is named' (Throws { Get-TestBootSeconds (Report 'ok' 'booted') 't' } 'has no time')
    Check 'a missing boot check is named' (Throws { Get-TestBootSeconds ([pscustomobject]@{ checks = @() }) 't' } "no 'test_boot' check")
    Check 'a fast boot passes the doctor check' (-not (Throws { Assert-DoctorGreen (Report 'ok' 'booted in 29.9 s') 't' $true } '.'))
    Check 'a slow boot fails the bar' (Throws { Assert-DoctorGreen (Report 'ok' 'booted in 30.1 s') 't' $true } 'over the 30 s bar')
    Check 'a skipped boot passes when no boot is required' (-not (Throws { Assert-DoctorGreen (Report 'skip' '--no-boot') 't' $false } '.'))
    Check 'a skipped boot fails when one is required' (Throws { Assert-DoctorGreen (Report 'skip' '--no-boot') 't' $true } "is 'skip'")
    Check 'Wait-Until returns what the probe found' ((Wait-Until 'x' 5 { 'found' } 10) -eq 'found')
    $calls = 0
    Check 'Wait-Until polls until the probe succeeds' ((Wait-Until 'x' 5 { $script:calls++; $script:calls -ge 3 } 10) -eq $true -and $calls -eq 3)
    Check 'Wait-Until gives up and says what it waited for' (Throws { Wait-Until 'the thing' 1 { $false } 100 } 'the thing \(gave up after 1 s\)')
    if ($failures -gt 0) { Fail "$failures self-test checks failed" }
    Write-Host 'self-test ok'
    return
}

Expect (Test-Path -LiteralPath $Msi) "no such MSI: $Msi"
if ($UpgradeMsi) { Expect (Test-Path -LiteralPath $UpgradeMsi) "no such MSI: $UpgradeMsi" }
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir exists before the test: not a clean machine"
Expect (-not (Test-Path -LiteralPath $DataDir)) "$DataDir exists before the test: not a clean machine"
$before = Get-Footprint

# Whether this machine can boot VMs, decided once. Where it cannot, the boot steps are skipped and
# the run says so (and fails on a pull request: a check that must not pass without running).
$vm = Get-VmFacts
Write-Host "HypervisorPresent=$($vm.Present) HypervisorPlatform=$($vm.Platform)"
$policy = Get-VmPolicy $vm.Present $vm.Platform $env:GITHUB_EVENT_NAME
$skipReason = "Windows Hypervisor Platform is not usable on this machine (HypervisorPresent=$($vm.Present), HypervisorPlatform=$($vm.Platform))"
if ($policy -eq 'fail') {
    Write-Host "::error title=msi boot cannot run::$skipReason; the boot steps must not be skipped on a pull request"
    Fail $skipReason
}
if ($policy -eq 'skip') {
    Write-Host "::warning title=msi boot skipped::$skipReason; the test boot and the workspace were not run (infrastructure, not a test result)"
    if ($env:GITHUB_STEP_SUMMARY) { "### msi: boot steps SKIPPED, $skipReason" | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY }
}

Step 'the package is per user'
foreach ($path in @($Msi, $UpgradeMsi) | Where-Object { $_ }) {
    Expect (Test-PerUserPackage $path) "$path is not a per-user package (needs no elevation, no ALLUSERS)"
}

Invoke-Msiexec @('/i', "`"$Msi`"") 'msi-install.log'
$first = Assert-InstalledLayout
Write-Host "installed version $($first.Version) in $InstallDir"
# The program must report the version of the package it came in (the kept MSI is named by it).
$reported = (& (Join-Path $InstallDir 'puddle.exe') --version) -join ' '
Expect ($reported -match [regex]::Escape($first.Version)) "puddle --version says '$reported' for package version $($first.Version)"
Assert-InstalledWorks $policy

if ($policy -eq 'run') {
    Step 'the installed program makes a workspace from the stock image, runs a command in it, stops and deletes it'
    $conn = Start-Installed
    try {
        Test-WorkspaceLifecycle $conn
    } finally {
        Stop-Installed
    }
}

Step 'the sign-in switch writes and removes the entry for the installed program'
Expect ($null -eq (Get-LoginEntry)) "a sign-in entry exists before the switch was turned on: $(Get-LoginEntry)"
$conn = Start-Installed
try {
    Set-StartAtLogin $conn $true
    Wait-LoginEntry (Get-ExpectedLoginEntry) 'turning the switch on wrote no entry for the installed program'
    Set-StartAtLogin $conn $false
    Wait-LoginEntry $null 'turning the switch off left the entry'
    # Left on for what follows: the upgrade and the uninstall must deal with a real entry.
    Set-StartAtLogin $conn $true
    Wait-LoginEntry (Get-ExpectedLoginEntry) 'turning the switch on again wrote no entry'
} finally {
    Stop-Installed
}

$current = $Msi
if ($UpgradeMsi) {
    Step 'upgrade keeps what the first run wrote'
    $marker = Join-Path $DataDir 'upgrade-marker.txt'
    Set-Content -LiteralPath $marker -Value ([guid]::NewGuid().ToString())
    $markerText = Get-Content -LiteralPath $marker -Raw
    $dataBefore = @(Get-ChildItem -LiteralPath $DataDir -Recurse -Force -File | ForEach-Object { $_.FullName } | Sort-Object)
    Expect ($dataBefore.Count -gt 1) 'the first run wrote nothing but the marker to the data folder'

    Invoke-Msiexec @('/i', "`"$UpgradeMsi`"") 'msi-upgrade.log'
    $second = Assert-InstalledLayout
    Expect ($second.Version -ne $first.Version) "the upgrade left version $($second.Version) installed"
    Write-Host "upgraded $($first.Version) to $($second.Version)"
    Expect ((Get-Content -LiteralPath $marker -Raw) -eq $markerText) 'the upgrade changed the marker in the data folder'
    # The upgrade removes the old version first; that must not take the sign-in entry with it
    # (checked before the program is started again: the program would write it back itself).
    Expect ((Get-LoginEntry) -eq (Get-ExpectedLoginEntry)) "the upgrade removed or changed the sign-in entry: '$(Get-LoginEntry)'"
    # The first run's files are still there (the connection file is rewritten by every start).
    $dataAfter = @(Get-ChildItem -LiteralPath $DataDir -Recurse -Force -File | ForEach-Object { $_.FullName })
    $lost = @($dataBefore | Where-Object { $dataAfter -notcontains $_ })
    Expect ($lost.Count -eq 0) "the upgrade removed data files: $($lost -join ', ')"
    Assert-InstalledWorks $policy
    $current = $UpgradeMsi
}

Expect ((Get-LoginEntry) -eq (Get-ExpectedLoginEntry)) 'the sign-in entry is not in place before the uninstall: the check below would prove nothing'
Invoke-Msiexec @('/x', "`"$current`"") 'msi-uninstall.log'
Step 'uninstall leaves nothing outside the data folders'
Stop-Installed
Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the uninstall: $(
    (Get-ChildItem -LiteralPath $InstallDir -Recurse -Force -ErrorAction SilentlyContinue | Select-Object -First 5 | ForEach-Object FullName) -join ', ')"
Expect (@(Get-UninstallEntries).Count -eq 0) 'the Add or remove programs entry is still there'
$strays = @(Get-Process -Name 'msb', 'puddle-app', 'puddle' -ErrorAction SilentlyContinue)
Expect ($strays.Count -eq 0) "the uninstall left Puddle processes running: $(($strays | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')"
Expect ($null -eq (Get-LoginEntry)) "the uninstall left the sign-in entry, which would start a program that is gone at every sign-in: $(Get-LoginEntry)"
$after = Get-Footprint
$added = @($after | Where-Object { $before -notcontains $_ })
$unexpected = @($added | Where-Object {
    $leaf = ($_ -replace '^\w+ ', '')
    -not ($_.StartsWith('top ') -and ($AllowedLeftovers -contains (Split-Path -Leaf $leaf)))
})
Expect ($unexpected.Count -eq 0) "the uninstall left: $($unexpected -join '; ')"
$removed = @($before | Where-Object { $after -notcontains $_ })
Write-Host "left behind on purpose: $(($added | ForEach-Object { $_ -replace '^\w+ ', '' }) -join ', ')"
Write-Host "gone from the machine since the start (not Puddle's): $($removed.Count)"
Step 'ok'
