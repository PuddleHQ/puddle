# SPDX-License-Identifier: GPL-3.0-or-later
# Installs Puddle's MSI on a clean Windows machine the way a user would (per user, silent), starts
# the installed program, runs the system check against the installed runtime, upgrades it with a
# newer MSI, and uninstalls it, checking at each step what the installer promises:
#   - the install is per user (the MSI says no elevation is needed; the files go under
#     %LOCALAPPDATA%\Programs\Puddle; nothing under Program Files; one Uninstall entry, in HKCU);
#   - the program, the CLI, the runtime folder (msb, firmware, guest agent, code-server) and the
#     licences are where the program looks for them;
#   - the installed program starts, answers its API, and its system check finds the runtime; the
#     CLI's `doctor --json` agrees;
#   - an upgrade over the older MSI keeps what the first run wrote in the data folder;
#   - uninstalling removes the install folder, the Start menu entry and the registry keys, and
#     leaves no file outside Puddle's own data folders (listed below).
# Usage: pwsh ci/msi-install-test.ps1 -Msi <first.msi> [-UpgradeMsi <newer.msi>] [-LogDir <dir>]
# Without -UpgradeMsi the upgrade step is skipped and the first MSI is the one uninstalled.
# A hosted runner has no usable hypervisor guarantee, so a failing `virtualization` or `hypervisor`
# check is reported but does not fail the test (it says nothing about the installer); every other
# failing check does.
param(
    [Parameter(Mandatory = $true)][string]$Msi,
    [string]$UpgradeMsi = '',
    [string]$LogDir = $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP })
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

# The tolerated checks: what a hosted runner may lack, unrelated to the installer.
$RunnerLimits = @('virtualization', 'hypervisor')

function Assert-DoctorGreen($Report, [string]$Source) {
    Expect ($Report.schema_version -ge 1) "${Source}: no schema_version in the report"
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
    Expect ($entries[0].Hive -eq 'HKCU') "the Add or remove programs entry is in $($entries[0].Hive), not HKCU"
    Expect (-not (Test-Path -LiteralPath (Join-Path $env:ProgramFiles $ProductName))) "something was installed under Program Files"
    $entries[0]
}

function Assert-InstalledWorks {
    $conn = Start-Installed
    try {
        $headers = @{ Authorization = "Bearer $($conn.token)" }
        $apiReport = Invoke-RestMethod -Uri "$($conn.url)/api/doctor?boot=false" -Headers $headers
        Assert-DoctorGreen $apiReport 'installed program, GET /api/doctor'
    } finally {
        Stop-Installed
    }
    $json = & (Join-Path $InstallDir 'puddle.exe') doctor --json --no-boot
    $cliReport = $json | ConvertFrom-Json
    Assert-DoctorGreen $cliReport 'puddle doctor --json --no-boot'
}

Expect (Test-Path -LiteralPath $Msi) "no such MSI: $Msi"
if ($UpgradeMsi) { Expect (Test-Path -LiteralPath $UpgradeMsi) "no such MSI: $UpgradeMsi" }
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir exists before the test: not a clean machine"
Expect (-not (Test-Path -LiteralPath $DataDir)) "$DataDir exists before the test: not a clean machine"
$before = Get-Footprint

Step 'the package is per user'
foreach ($path in @($Msi, $UpgradeMsi) | Where-Object { $_ }) {
    Expect (Test-PerUserPackage $path) "$path is not a per-user package (needs no elevation, no ALLUSERS)"
}

Invoke-Msiexec @('/i', "`"$Msi`"") 'msi-install.log'
$first = Assert-InstalledLayout
Write-Host "installed version $($first.Version) in $InstallDir"
Assert-InstalledWorks

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
    # The first run's files are still there (the connection file is rewritten by every start).
    $dataAfter = @(Get-ChildItem -LiteralPath $DataDir -Recurse -Force -File | ForEach-Object { $_.FullName })
    $lost = @($dataBefore | Where-Object { $dataAfter -notcontains $_ })
    Expect ($lost.Count -eq 0) "the upgrade removed data files: $($lost -join ', ')"
    Assert-InstalledWorks
    $current = $UpgradeMsi
}

Invoke-Msiexec @('/x', "`"$current`"") 'msi-uninstall.log'
Step 'uninstall leaves nothing outside the data folders'
Stop-Installed
Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the uninstall: $(
    (Get-ChildItem -LiteralPath $InstallDir -Recurse -Force -ErrorAction SilentlyContinue | Select-Object -First 5 | ForEach-Object FullName) -join ', ')"
Expect (@(Get-UninstallEntries).Count -eq 0) 'the Add or remove programs entry is still there'
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
