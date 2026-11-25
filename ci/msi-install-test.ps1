# SPDX-License-Identifier: AGPL-3.0-or-later
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
# With -BaselineMsi it tests instead the upgrade a user makes: from an older build (the last
# release, ci/fetch-baseline-msi.sh) to this one, with a workspace that holds files:
#   - the older build makes a workspace from the stock image and writes files to its volume, and
#     the workspace is stopped, as a user leaves it before an upgrade;
#   - after the upgrade the newer build lists the workspace under its id and name, its system check
#     passes against the older runtime's data (msb's home included), the workspace starts, and its
#     files are there byte for byte, read through the newer runtime;
#   - the older build also holds what a user builds up (crates/puddle-app/e2e/upgrade-state.mjs): a global allow
#     rule, a workspace deny rule, a pending request, an environment variable and a secret, three settings, an
#     identity and a file in the workspace folder, and every GET of the playbook is dumped; after the quit and the upgrade
#     every dump is equal, the build is not older, the guest sees the variable, the stand-in and the file, approving
#     the kept request lets the guest's retry return 200, the editor's server answers, and each main screen loads
#     with no error alert;
#   - the workspace is deleted and the newer build uninstalled.
# The steps before the upgrade drive the older build's API, so they use only the calls both have.
#   - with -UpgradeMsi, an upgrade while the program runs, and again with a workspace running, is
#     refused (the old install byte for byte as it was, nothing queued for a restart, the message
#     in the log); once Puddle is quit the upgrade goes through.
#   - the first install is made the way a person makes it, with the installer's own dialogs, driven
#     through UI Automation: each dialog's text is logged (msi-install-dialogs.log), none has a raw
#     "[1]" placeholder, and the whole install ends in under 60 s; the Start menu shortcut exists, targets
#     puddle-app.exe and opens a window on "Welcome to Puddle"; an interactive upgrade while the program
#     runs shows the refusal in a dialog that UI Automation reads; a launch with --at-login (the command
#     of the sign-in entry) has a tray icon and no visible window; an uninstall with a stopped workspace
#     that holds files keeps the data folder and the volume, and a reinstall lists the same workspace id
#     with its files byte for byte.
# Usage: pwsh ci/msi-install-test.ps1 -Msi <first.msi> [-UpgradeMsi <newer.msi>] [-LogDir <dir>] [-PlantFootprint <breakage>]
# -PlantFootprint (no-shortcut, wrong-shortcut-target, visible-at-login, data-lost, slow-install, no-refusal, kill-skipped)
# makes one of those footprint checks see the breakage it guards against; the run must then fail (the msi
# workflow's dispatch input plant_footprint runs it so and expects the failure).
#        pwsh ci/msi-install-test.ps1 -BaselineMsi <older.msi> -UpgradeMsi <newer.msi> [-LogDir <dir>] [-PlantBreakage drop-table|empty-env|empty-pending]
# -PlantBreakage damages the older build's data after its quit, as a broken migration would; the run must then
# fail (the msi workflow's dispatch input plant_breakage runs it so and expects the failure).
#        pwsh ci/msi-install-test.ps1 -SelfTest      (the helpers' own checks; needs no MSI)
# Without -UpgradeMsi the upgrade step is skipped and the first MSI is the one uninstalled.
# Where the machine has no usable hypervisor, a failing `virtualization` or `hypervisor` check is
# reported but does not fail the test (it says nothing about the installer), the boot steps are
# skipped and say so (with -BaselineMsi that is the whole test); every other failing check fails.
[CmdletBinding(DefaultParameterSetName = 'Run')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Run')][string]$Msi,
    [Parameter(Mandatory = $true, ParameterSetName = 'CrossVersion')][string]$BaselineMsi,
    [Parameter(ParameterSetName = 'Run')]
    [Parameter(Mandatory = $true, ParameterSetName = 'CrossVersion')][string]$UpgradeMsi = '',
    [Parameter(ParameterSetName = 'Run')]
    [Parameter(ParameterSetName = 'CrossVersion')][string]$LogDir = $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }),
    [Parameter(ParameterSetName = 'CrossVersion')][ValidateSet('', 'drop-table', 'empty-env', 'empty-pending')][string]$PlantBreakage = '',
    [Parameter(ParameterSetName = 'Run')][ValidateSet('', 'no-shortcut', 'wrong-shortcut-target', 'visible-at-login', 'data-lost', 'slow-install', 'no-refusal', 'kill-skipped')][string]$PlantFootprint = '',
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

# The rows of the MSI's ActionText table that still hold a raw placeholder such as "[1]" in the
# text or the template of the "Installing" page (Windows Installer fills those per item; WixUI's
# templates showed "File: [1], Directory: [9], Size: [6]" as written). Each row is named
# "Action: text | template".
function Find-PlaceholderRows($Rows) {
    foreach ($row in $Rows) {
        if ("$($row.Description)$($row.Template)" -match '\[[0-9A-Za-z_%#!$]*\]') {
            "$($row.Action): $($row.Description) | $($row.Template)"
        }
    }
}

# The actions this installer runs whose built-in text carries a per-item placeholder. Each needs its
# own plain text in the package (crates/puddle-app/wix/main.wxs), or Windows Installer shows its
# built-in "Folder: [1]" style text for it.
$ActionsWithText = @('InstallFiles', 'CreateFolders', 'RemoveFolders', 'RemoveFiles', 'CreateShortcuts', 'RemoveShortcuts',
    'WriteRegistryValues', 'RemoveRegistryValues', 'RegisterProduct', 'PublishProduct', 'PublishFeatures', 'UnpublishFeatures',
    'RemoveExistingProducts', 'ProcessComponents', 'FindRelatedProducts', 'AppSearch', 'MigrateFeatureStates', 'RollbackCleanup',
    'InstallAdminPackage')

# The actions of $ActionsWithText that have no row, then every row with a raw placeholder.
function Find-InstallerTextProblems($Rows) {
    foreach ($action in $ActionsWithText) {
        if (-not (@($Rows) | Where-Object { $_.Action -eq $action })) { "$action has no plain text (the built-in raw text is shown)" }
    }
    foreach ($raw in Find-PlaceholderRows $Rows) { "raw placeholder: $raw" }
}

# Every row of the MSI's ActionText table as Action, Description, Template.
function Get-ActionTextRows([string]$Path) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $Path).Path, 0))
    # A package that sets no text has no ActionText table at all, and then shows the built-in raw texts.
    try {
        $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @('SELECT Action, Description, Template FROM ActionText'))
    } catch { return }
    $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
    while ($true) {
        $row = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
        if ($null -eq $row) { break }
        $field = { param($i) $row.GetType().InvokeMember('StringData', 'GetProperty', $null, $row, @($i)) }
        [pscustomobject]@{ Action = (& $field 1); Description = (& $field 2); Template = (& $field 3) }
    }
    $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
}

function Invoke-Msiexec([string[]]$Arguments, [string]$LogName) {
    $log = Join-Path $LogDir $LogName
    $all = $Arguments + @('/qn', '/norestart', '/l*v', "`"$log`"")
    Step "msiexec $($Arguments -join ' ')  (log: $log)"
    $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList $all -Wait -PassThru
    Expect ($p.ExitCode -eq 0) "msiexec $($Arguments -join ' ') exited with $($p.ExitCode); see $log"
}

# Runs msiexec on a package that must refuse: returns the exit code and the log's text. A run
# that succeeds is the failure.
function Invoke-MsiexecRefused([string[]]$Arguments, [string]$LogName) {
    $log = Join-Path $LogDir $LogName
    $all = $Arguments + @('/qn', '/norestart', '/l*v', "`"$log`"")
    Step "msiexec $($Arguments -join ' '), expecting a refusal  (log: $log)"
    $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList $all -Wait -PassThru
    $text = if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Raw } else { '' }
    [pscustomobject]@{ Code = $p.ExitCode; Log = $text }
}

# Windows Installer shows a shortcut's or an Add-or-remove-programs entry's icon from a file it
# writes under the Icon table's name, and picks the file's type from the name's extension: a name
# without ".ico" or ".exe" (WiX's default is the bare Id) leaves a shortcut with a generic icon.
# Returns the Icon table's names and each shortcut's icon reference.
function Get-PackageIcons([string]$Path) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember('OpenDatabase', 'InvokeMethod', $null, $installer, @((Resolve-Path $Path).Path, 0))
    function Select-Column([string]$Query) {
        $view = $db.GetType().InvokeMember('OpenView', 'InvokeMethod', $null, $db, @($Query))
        $view.GetType().InvokeMember('Execute', 'InvokeMethod', $null, $view, $null) | Out-Null
        while ($true) {
            $row = $view.GetType().InvokeMember('Fetch', 'InvokeMethod', $null, $view, $null)
            if ($null -eq $row) { break }
            $row.GetType().InvokeMember('StringData', 'GetProperty', $null, $row, @(1))
        }
        $view.GetType().InvokeMember('Close', 'InvokeMethod', $null, $view, $null) | Out-Null
    }
    [pscustomobject]@{
        Icons = @(Select-Column 'SELECT `Name` FROM `Icon`')
        ShortcutIcons = @(Select-Column 'SELECT `Icon_` FROM `Shortcut` WHERE `Icon_` IS NOT NULL')
        ProductIcon = @(Select-Column "SELECT `Value` FROM `Property` WHERE `Property`='ARPPRODUCTICON'")
    }
}

function Assert-PackageIcons([string]$Path) {
    $icons = Get-PackageIcons $Path
    Expect ($icons.Icons.Count -ge 1) "$Path has no Icon table entry"
    foreach ($name in $icons.Icons) {
        Expect ($name -match '\.(ico|exe)$') "$Path names an icon '$name' without an .ico or .exe extension, so Windows shows the shortcut's icon as a generic one"
    }
    Expect ($icons.ShortcutIcons.Count -ge 1) "$Path has a shortcut with no icon"
    foreach ($name in @($icons.ShortcutIcons) + @($icons.ProductIcon)) {
        Expect ($icons.Icons -contains $name) "$Path refers to icon '$name', which its Icon table does not hold"
    }
}

Add-Type -AssemblyName System.Drawing
Add-Type -Namespace Puddle -Name Icons -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
public static extern uint PrivateExtractIcons(string file, int index, int cx, int cy, System.IntPtr[] icons, uint[] ids, uint count, uint flags);
[System.Runtime.InteropServices.DllImport("user32.dll")]
public static extern bool DestroyIcon(System.IntPtr icon);
'@

# The 256 px image of the icon a file carries (an .exe's first icon resource, or an .ico), as a bitmap.
function Get-FileIconBitmap([string]$File) {
    $handles = New-Object 'System.IntPtr[]' 1
    $ids = New-Object 'uint32[]' 1
    $got = [Puddle.Icons]::PrivateExtractIcons($File, 0, 256, 256, $handles, $ids, 1, 0)
    Expect ($got -ge 1) "$File carries no icon"
    try {
        $icon = [System.Drawing.Icon]::FromHandle($handles[0])
        $bitmap = New-Object System.Drawing.Bitmap 256, 256
        $g = [System.Drawing.Graphics]::FromImage($bitmap)
        $g.Clear([System.Drawing.Color]::Transparent)
        $g.DrawIcon($icon, (New-Object System.Drawing.Rectangle 0, 0, 256, 256))
        $g.Dispose()
        return $bitmap
    } finally { [void][Puddle.Icons]::DestroyIcon($handles[0]) }
}

# Share of pixels that differ clearly (a channel off by more than 24) between two 256 px bitmaps.
function Get-IconDifference([System.Drawing.Bitmap]$A, [System.Drawing.Bitmap]$B) {
    $bad = 0
    for ($y = 0; $y -lt 256; $y += 2) {
        for ($x = 0; $x -lt 256; $x += 2) {
            $p = $A.GetPixel($x, $y); $q = $B.GetPixel($x, $y)
            if ([math]::Abs($p.A - $q.A) -gt 24 -or [math]::Abs($p.R - $q.R) -gt 24 -or
                [math]::Abs($p.G - $q.G) -gt 24 -or [math]::Abs($p.B - $q.B) -gt 24) { $bad++ }
        }
    }
    $bad / (128 * 128)
}

# The duck: crates/puddle-app/icons/icon.png, which crates/puddle-appicon/tests/installed_icon.rs
# pins to the drawing the tray uses.
function Get-DuckBitmap {
    $png = Join-Path $PSScriptRoot '..\crates\puddle-app\icons\icon.png'
    New-Object System.Drawing.Bitmap ((Resolve-Path $png).Path)
}

function Assert-FileIsDuck([string]$File, [string]$What) {
    $diff = Get-IconDifference (Get-FileIconBitmap $File) (Get-DuckBitmap)
    Expect ($diff -lt 0.02) ("{0}'s icon ({1}) is not the duck: {2:P1} of its pixels differ" -f $What, $File, $diff)
}

# What a user sees of the install: the program's own icon, and the icon file of the Start menu
# shortcut, are the duck.
function Assert-InstalledIcon {
    Assert-FileIsDuck (Join-Path $InstallDir 'puddle-app.exe') 'the installed program'
    $link = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$ProductName\$ProductName.lnk"
    Expect (Test-Path -LiteralPath $link) "the Start menu has no $link"
    $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($link)
    $location = [Environment]::ExpandEnvironmentVariables(($shortcut.IconLocation -replace ',-?\d+$', ''))
    Expect (Test-Path -LiteralPath $location) "the Start menu shortcut's icon file $location does not exist"
    Assert-FileIsDuck $location 'the Start menu shortcut'
}

# Path, size and SHA-256 of every file under the install folder, sorted: two listings are equal
# when the folder is byte for byte the same.
function Get-TreeDigest {
    @(Get-ChildItem -LiteralPath $InstallDir -Recurse -Force -File | Sort-Object FullName | ForEach-Object {
        "$($_.FullName.Substring($InstallDir.Length)) $($_.Length) $((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash)"
    })
}

# Names, as 'path' lines, of any install-folder file queued for replacement at the next restart.
function Get-PendingReplacements {
    $session = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager'
    $queued = (Get-ItemProperty -LiteralPath $session -ErrorAction SilentlyContinue).PendingFileRenameOperations
    @($queued | Where-Object { $_ -and $_.IndexOf($InstallDir, [StringComparison]::OrdinalIgnoreCase) -ge 0 })
}

# The sentence the installer shows (and logs) when it refuses to run over a running Puddle (main.wxs, PuddleRunning).
$RefusalText = 'Puddle is running. Quit Puddle from its tray icon (this stops your workspaces), then run this installer again.'

# The refusal: the newer package changes nothing while the program or a workspace runs. $Why
# names the situation for the messages. The caller keeps the program (and workspace) running.
function Assert-UpgradeRefused([string]$UpgradeMsi, [string]$Why, [string]$LogName, [string]$FirstVersion) {
    $digest = Get-TreeDigest
    $running = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } | ForEach-Object Id | Sort-Object)
    $refused = Invoke-MsiexecRefused @('/i', "`"$UpgradeMsi`"") $LogName
    Expect ($refused.Code -ne 0) "the upgrade went ahead $Why; it must refuse"
    Expect ($refused.Log.Contains($RefusalText)) "the refusal $Why does not say to quit Puddle; see $(Join-Path $LogDir $LogName)"
    $same = Get-TreeDigest
    Expect (($same -join "`n") -eq ($digest -join "`n")) "the refused upgrade $Why changed the install folder"
    $entries = @(Get-UninstallEntries)
    Expect ($entries.Count -eq 1 -and $entries[0].Version -eq $FirstVersion) "the refused upgrade $Why changed the installed version: $(($entries | ForEach-Object Version) -join ', ')"
    $still = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } | ForEach-Object Id | Sort-Object)
    Expect (($still -join ',') -eq ($running -join ',')) "the refused upgrade $Why ended a Puddle process (before: $($running -join ','), after: $($still -join ','))"
    $pending = @(Get-PendingReplacements)
    Expect ($pending.Count -eq 0) "the refused upgrade $Why queued files for the next restart: $($pending -join '; ')"
    Write-Host "refused $Why (exit $($refused.Code)); the install folder is byte for byte as it was, nothing queued for a restart"
}

# The refusal of a downgrade: the lower package changes nothing. The installed program is the one
# of version $InstalledVersion. Tauri's WiX template words it as DowngradeErrorMessage does.
$DowngradeRefusalText = 'A newer version of Puddle is already installed.'
function Assert-DowngradeRefused([string]$LowerMsi, [string]$InstalledVersion, [string]$LogName) {
    $digest = Get-TreeDigest
    $refused = Invoke-MsiexecRefused @('/i', "`"$LowerMsi`"") $LogName
    Expect ($refused.Code -ne 0) 'the lower version installed over the higher one; it must refuse'
    Expect ($refused.Log.Contains($DowngradeRefusalText)) "the downgrade was refused without saying '$DowngradeRefusalText'; see $(Join-Path $LogDir $LogName)"
    $same = Get-TreeDigest
    Expect (($same -join "`n") -eq ($digest -join "`n")) 'the refused downgrade changed the install folder'
    $entries = @(Get-UninstallEntries)
    Expect ($entries.Count -eq 1 -and $entries[0].Version -eq $InstalledVersion) "the refused downgrade changed the installed version: $(($entries | ForEach-Object Version) -join ', ')"
    $pending = @(Get-PendingReplacements)
    Expect ($pending.Count -eq 0) "the refused downgrade queued files for the next restart: $($pending -join '; ')"
    Write-Host "refused the downgrade (exit $($refused.Code)); the install folder is byte for byte as it was"
}

function Stop-Installed {
    Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { Stop-Process -Id $_.Id -Force; $_.WaitForExit(30000) | Out-Null }
}

# Starts the installed program, waits for its connection file, and returns the API's connection.
function Start-Installed([string[]]$Arguments = @()) {
    $connectionFile = Join-Path $DataDir 'api.json'
    Remove-Item -LiteralPath $connectionFile -Force -ErrorAction SilentlyContinue
    $start = @{ FilePath = (Join-Path $InstallDir 'puddle-app.exe'); PassThru = $true }
    if ($Arguments.Count -gt 0) { $start.ArgumentList = $Arguments }
    $app = Start-Process @start
    $script:StartedAppPid = $app.Id
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

# $Baseline: the older release being upgraded from. It predates some of today's files (the
# licence texts), so only the files every release shipped are required of it.
function Assert-InstalledLayout([switch]$Baseline) {
    $required = @(
        'puddle-app.exe', 'puddle.exe',
        'runtime\msb.exe', 'runtime\libkrunfw.dll', 'runtime\runtime.json', 'runtime\puddle-agent',
        'runtime\code-server\bin\code-server', 'runtime\code-server\package.json')
    if (-not $Baseline) { $required += @(
        'runtime\licenses\NOTICE.txt', 'runtime\licenses\AGPL-3.0.txt',
        'runtime\licenses\THIRD-PARTY-msb.txt', 'runtime\licenses\THIRD-PARTY-puddle.txt',
        'runtime\licenses\THIRD-PARTY-ui.txt', 'runtime\licenses\code-server\LICENSE') }
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

function Get-WorkspaceNow($Conn, [string]$Id) {
    (Invoke-PuddleApi $Conn 'Get' '/api/workspaces').workspaces | Where-Object { $_.id -eq $Id }
}

# Waits until workspace $Id runs and is not busy, and returns it. A workspace that is made or
# stopped and not busy is started once, as the app's own button does; crashed or volume_missing
# fails at once.
function Wait-WorkspaceRunning($Conn, [string]$Id, [string]$Name, [int]$TimeoutSec) {
    # A hashtable, so the probe's change is seen by its next call (a scriptblock's own variables
    # are new each time).
    $state = @{ Started = $false }
    Wait-Until "workspace $Name ($Id) is not running" $TimeoutSec {
        $w = Get-WorkspaceNow $Conn $Id
        if ($null -eq $w) { Fail "workspace $Name ($Id) vanished while it was being made or started" }
        if ($w.status -in @('crashed', 'volume_missing')) { Fail "workspace $Name ($Id) is $($w.status)" }
        if ($w.status -eq 'running' -and -not $w.busy) { return $w }
        if (-not $state.Started -and -not $w.busy -and $w.status -in @('created', 'stopped')) {
            $state.Started = $true
            Invoke-PuddleApi $Conn 'Post' "/api/workspaces/$Id/start" | Out-Null
        }
    }
}

function Stop-Workspace($Conn, [string]$Id, [string]$Name) {
    Invoke-PuddleApi $Conn 'Post' "/api/workspaces/$Id/stop" | Out-Null
    Wait-Until "workspace $Name ($Id) did not stop" 120 {
        $w = Get-WorkspaceNow $Conn $Id
        $null -ne $w -and -not $w.busy -and $w.status -eq 'stopped'
    } | Out-Null
}

# Deletes a stopped workspace and checks that nothing of it runs or stays in the runtime.
function Remove-Workspace($Conn, [string]$Id, [string]$Name) {
    # The delete check's fingerprint is the user's "I reviewed this": a workspace holding data
    # outside a checkout (the upgrade test's files) is refused without it.
    $check = Invoke-PuddleApi $Conn 'Get' "/api/workspaces/$Id/delete-check" -TimeoutSec 120
    Invoke-PuddleApi $Conn 'Delete' "/api/workspaces/$Id" @{ confirm = $true; fingerprint = $check.fingerprint } | Out-Null
    Wait-Until "workspace $Name ($Id) is still listed after the delete" 120 { $null -eq (Get-WorkspaceNow $Conn $Id) } | Out-Null
    Wait-Until 'a runtime process is still running after the workspace was deleted' 60 {
        @(Get-RuntimeProcesses).Count -eq 0
    } | Out-Null
    $left = Invoke-InstalledMsb @('ls')
    Expect ($left.Output -notmatch [regex]::Escape($Id)) "msb still lists $Id after the delete: $($left.Output)"
    Write-Host "workspace $Name ($Id) stopped and deleted; no runtime process left"
}

# Runs a shell command in workspace $Id through the installed msb and returns its output; fails
# naming $What when it exits non-zero.
function Invoke-InWorkspace([string]$Id, [string]$Command, [string]$What) {
    $ran = Invoke-InstalledMsb @('exec', $Id, '--', 'sh', '-c', $Command)
    Expect ($ran.Code -eq 0) "${What}: msb exec in $Id exited with $($ran.Code): $($ran.Output)"
    $ran.Output
}

# A workspace from the stock image, made, run in, stopped and deleted through the installed
# program's API, the way the app's own buttons do. Puddle picks the id; the create's answer
# carries it, and every later call and the runtime's sandbox go by it.
function Test-WorkspaceLifecycle($Conn) {
    $name = 'msi-test'
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $wsId = New-Workspace $Conn $name
    $running = Wait-WorkspaceRunning $Conn $wsId $name $CreateBudgetSec
    Write-Host "workspace $name ($wsId) running $([math]::Round($clock.Elapsed.TotalSeconds)) s after the create (image: $($running.image))"
    Expect ($clock.Elapsed.TotalSeconds -le $CreateBudgetSec) "the create took over $CreateBudgetSec s"

    $token = [guid]::NewGuid().ToString()
    $out = Invoke-InWorkspace $wsId "echo $token; . /etc/os-release; echo `$ID" 'the command'
    Expect ($out -match [regex]::Escape($token)) "the command in $wsId did not run: $out"
    Expect ($out -match '(?m)^debian\s*$') "the workspace is not the stock debian image: $out"
    Write-Host "ran a command inside $wsId"

    Stop-Workspace $Conn $wsId $name
    Remove-Workspace $Conn $wsId $name
}

# The buttons of the notification area (the tray) named after Puddle: what a person sees of a
# running app beside the clock. Windows 11 puts them in a XAML island (class SystemTray.NormalButton),
# older shells in a toolbar of Shell_TrayWnd or of the overflow window. A new icon may sit in the
# hidden-icons overflow, which UI Automation lists only while it is open, so the overflow is opened
# first (the "Show Hidden Icons" button, as a person would) when the taskbar has one.
# With -Hover the pointer passes over each icon found (Windows prunes the icon of a program that is
# gone when the pointer reaches it). Returns the icons found before any hover; the overflow is
# closed again.
function Get-PuddleTrayIcons([switch]$Hover) {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $A = [System.Windows.Automation.AutomationElement]
    $buttons = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    $trays = @('Shell_TrayWnd', 'NotifyIconOverflowWindow', 'TopLevelWindowForOverflowXamlIsland')
    $find = {
        @($A::RootElement.FindAll('Children', [System.Windows.Automation.Condition]::TrueCondition) |
            Where-Object { $_.Current.ClassName -in $trays } |
            ForEach-Object { $_.FindAll('Descendants', $buttons) })
    }
    $all = & $find
    $chevron = @($all | Where-Object { $_.Current.Name -eq 'Show Hidden Icons' }) | Select-Object -First 1
    if ($chevron) {
        $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Seconds 2
        $all = & $find
    }
    $icons = @($all | Where-Object { $_.Current.Name -match '^Puddle(:|$)' })
    if ($Hover) {
        foreach ($icon in $icons) {
            $r = $icon.Current.BoundingRectangle
            [void][TrayCursor]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
            Start-Sleep -Milliseconds 500
        }
        [void][TrayCursor]::SetCursorPos(0, 0)
    }
    if ($chevron) {
        $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 500
    }
    $icons
}

Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
public static class TrayCursor {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
}
'@

# Work worth losing: a repository with 21 commits, a staged and
# an unstaged file, in the workspace's folder, synced to the volume.
function Get-MakeWorkScript([string]$Folder) {
    "set -e; cd $Folder; git init -q -b main qa-crash; cd qa-crash; git config user.name 'Puddle Test'; " +
    "git config user.email test@puddle.invalid; git config commit.gpgsign false; " +
    "echo 0 > qa.txt; git add qa.txt; git commit -qm 'qa 0'; " +
    "for i in `$(seq 1 20); do echo `$i >> qa.txt; git add qa.txt; git commit -qm `"qa `$i`"; done; " +
    "echo staged > staged.txt; git add staged.txt; echo unstaged > unstaged.txt; sync; " +
    "echo count `$(git rev-list --count HEAD)"
}

function Get-ReadWorkScript([string]$Folder) {
    "cd $Folder/qa-crash || exit 3; echo count `$(git rev-list --count HEAD); " +
    "if git fsck --full >/tmp/fsck.out 2>&1; then echo 'fsck clean'; else echo 'fsck failed'; cat /tmp/fsck.out; fi; " +
    "if git diff --cached --name-only | grep -qx staged.txt; then echo 'staged kept'; else echo 'staged lost'; fi; " +
    "if [ -f unstaged.txt ]; then echo 'unstaged kept'; else echo 'unstaged lost'; fi"
}

# A hard kill of the app, with no person: the installed app (puddle-app.exe) is ended with
# TerminateProcess (Stop-Process -Force), as Task Manager's End task does, while a workspace with
# work in it runs. Every runtime process it started (msb and the VMs) is gone within 5 s: the
# kill-on-close job object ends them. The tray icon goes (Windows prunes a dead program's icon when
# the pointer passes over it, which is what a person sees, so the test passes the pointer over it).
# The next start shows the workspace crashed or stopped, a start runs it, and the repository has its
# 21 commits, a clean fsck, and its staged file still staged or a problem that says it was lost.
function Test-HardKill {
    $name = 'qa-crash'
    $conn = Start-Installed
    $wsId = New-Workspace $conn $name
    Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
    $folder = "/workspace"
    $made = Invoke-InWorkspace $wsId (Get-MakeWorkScript $folder) 'making the work'
    Expect ($made -match '(?m)^count 21\s*$') "the work was not made: $made"
    $runtime = @(Get-RuntimeProcesses)
    Expect ($runtime.Count -gt 0) 'no runtime process runs with the workspace, so the kill below would prove nothing'
    Expect (@(Get-PuddleTrayIcons).Count -ge 1) 'the running app shows no tray icon, so the check after the kill would prove nothing'
    $app = @(Get-Process -Name 'puddle-app' -ErrorAction Stop | Sort-Object StartTime)[0]

    Step "TerminateProcess on puddle-app.exe ($($app.Id)) with $($runtime.Count) runtime processes running"
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    # -PlantFootprint kill-skipped: the app is not ended, as if the kill did not reach it; the
    # runtime processes then outlive the 5 s bar and the step must fail.
    if ($PlantFootprint -ne 'kill-skipped') { Stop-Process -Id $app.Id -Force }
    $ids = @($runtime | ForEach-Object { $_.Id })
    Wait-Until "runtime processes still run 5 s after the kill: $(($runtime | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')" 5 {
        @(Get-Process -Id $ids -ErrorAction SilentlyContinue).Count -eq 0
    } -PollMs 100 | Out-Null
    Write-Host "every runtime process was gone $([math]::Round($clock.Elapsed.TotalSeconds, 1)) s after the kill"
    Expect (@(Get-Process -Name 'puddle-app' -ErrorAction SilentlyContinue).Count -eq 0) 'a puddle-app process is left after the kill'
    $ghosts = @(Get-PuddleTrayIcons -Hover)
    if ($ghosts.Count -gt 0) {
        Write-Host "INFO: $($ghosts.Count) dead tray icon(s) stayed until the pointer passed over them (a Windows habit)"
    }
    Wait-Until 'a tray icon of the killed app is left' 10 { @(Get-PuddleTrayIcons).Count -eq 0 } -PollMs 500 | Out-Null

    Step 'the next start shows the workspace crashed or stopped, and a start brings back the work'
    $conn = Start-Installed
    try {
        $after = Get-WorkspaceNow $conn $wsId
        Expect ($null -ne $after) "the workspace $wsId is not listed after the kill"
        Expect ($after.status -in @('crashed', 'stopped')) "after the kill the workspace shows $($after.status)"
        Expect (@(Get-PuddleTrayIcons).Count -eq 1) "the started app shows $(@(Get-PuddleTrayIcons).Count) tray icons, not one"
        Invoke-PuddleApi $conn 'Post' "/api/workspaces/$wsId/start" | Out-Null
        Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
        $read = Invoke-InWorkspace $wsId (Get-ReadWorkScript $folder) 'reading the work'
        Write-Host $read
        Expect ($read -match '(?m)^count 21\s*$') "commits were lost in the kill: $read"
        Expect ($read -match '(?m)^fsck clean\s*$') "git fsck finds damage after the kill: $read"
        if ($read -match '(?m)^staged lost\s*$') {
            $problems = @((Invoke-PuddleApi $conn 'Get' '/api/problems').problems)
            $said = @($problems | Where-Object { $_.workspace -eq $wsId -and $_.detail -match 'no longer staged' })
            Expect ($said.Count -gt 0) "the staged file was lost and no problem says so: $($problems | ConvertTo-Json -Compress -Depth 4)"
        }
        Stop-Workspace $conn $wsId $name
        Remove-Workspace $conn $wsId $name
    } finally {
        Stop-Installed
    }
}

# Makes a workspace from the stock image over the API and returns the id Puddle gave it.
function New-Workspace($Conn, [string]$Name) {
    $made = Invoke-PuddleApi $Conn 'Post' '/api/workspaces' @{ name = $Name }
    Expect ($made.id -cmatch '^[a-z][0-9a-z]{7}$') "the create answered no generated id: $($made | ConvertTo-Json -Compress)"
    Expect ($made.name -eq $Name) "the create answered the name $($made.name), not $Name"
    $made.id
}

# The files the upgrade must keep, in the folder $Dir of the workspace's volume: a token the
# test chose and 4 MiB of random bytes. The script fails unless $Dir's parent is a mount (the
# volume, not the sandbox's own disk), and prints the token and the bytes' SHA-256.
function Get-WriteFilesScript([string]$VolumeRoot, [string]$Token) {
    $dir = "$VolumeRoot/upgrade-check"
    "set -e; grep -q ' $VolumeRoot ' /proc/mounts || { echo '$VolumeRoot is not a mount'; exit 1; }; " +
    "mkdir -p $dir; echo $Token > $dir/token.txt; head -c 4194304 /dev/urandom > $dir/random.bin; sync; " +
    "cat $dir/token.txt; sha256sum $dir/random.bin"
}

# Reads back what Get-WriteFilesScript wrote.
function Get-ReadFilesScript([string]$VolumeRoot) {
    $dir = "$VolumeRoot/upgrade-check"
    "set -e; cat $dir/token.txt; sha256sum $dir/random.bin"
}

# The token and the SHA-256 from the output of either script, or $null for what is missing.
function Read-FilesReport([string]$Output) {
    $lines = @($Output -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    $sha = $lines | Where-Object { $_ -match '^([0-9a-f]{64})\s' } | ForEach-Object { $Matches[1] } | Select-Object -First 1
    $token = $lines | Where-Object { $_ -match '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' } | Select-Object -First 1
    [pscustomobject]@{ Token = $token; Sha256 = $sha }
}

# Fails unless the package version $To is newer than $From: Windows Installer only upgrades to a
# higher version, so anything else would test a downgrade or a reinstall.
function Assert-NewerVersion([string]$From, [string]$To) {
    Expect ([version]$To -gt [version]$From) "the upgrade goes from $From to $To, which is not newer"
}

# Runs crates/puddle-app/e2e/upgrade-state.mjs (the state an upgrade must keep) against the running
# installed program; fails when any of its checks fails. Its lines are the job's log.
function Invoke-UpgradeState([string[]]$Arguments, [string]$Phase) {
    $script = Join-Path $PSScriptRoot '..\crates\puddle-app\e2e\upgrade-state.mjs'
    Expect (Test-Path -LiteralPath $script) "no $script"
    Step "upgrade state, $Phase"
    $app = Get-Process -Name 'puddle-app' -ErrorAction SilentlyContinue | Select-Object -First 1
    $env:PUDDLE_APP_PID = if ($app) { "$($app.Id)" } else { '' }
    $env:PUDDLE_INSTALL_DIR = $InstallDir
    $lines = @(& node $script @Arguments 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
    $lines | ForEach-Object { Write-Host $_ }
    $lines | Set-Content -LiteralPath (Join-Path $LogDir "upgrade-state-$Phase.log")
    Expect ($code -eq 0) "the upgrade-state $Phase step failed (exit $code): $(($lines | Where-Object { $_ -match '^FAIL' }) -join ' | ')"
}

# The upgrade a user makes: from the older build $Baseline to $Upgrade, with a workspace that
# holds files. $Policy is 'run' or 'skip' (no usable hypervisor: nothing here can run).
function Test-UpgradeFromBaseline([string]$Baseline, [string]$Upgrade, [string]$Policy) {
    if ($Policy -ne 'run') {
        Write-Host '::warning title=upgrade test skipped::the upgrade with a workspace needs a running workspace, and this machine boots none'
        return
    }
    Invoke-Msiexec @('/i', "`"$Baseline`"") 'msi-baseline-install.log'
    $first = Assert-InstalledLayout -Baseline
    Write-Host "installed the baseline, version $($first.Version)"

    Step 'the older build makes a workspace and writes files to its volume, then stops it'
    $name = 'upgrade-test'
    $StateDir = Join-Path $LogDir 'upgrade-state'
    $token = [guid]::NewGuid().ToString()
    $conn = Start-Installed
    try {
        $wsId = New-Workspace $conn $name
        Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
        # A baseline from before the single guest folder mounts the volume at /workspaces/<id>;
        # this build mounts it at /workspace and makes such a sandbox again at its first start.
        $where = Invoke-InWorkspace $wsId "if grep -q ' /workspace ' /proc/mounts; then echo /workspace; else echo /workspaces/$wsId; fi" 'finding the volume'
        $volume = @($where | ForEach-Object { $_ -split "`n" } | ForEach-Object { $_.Trim() } | Where-Object { $_ })[-1]
        $wrote = Read-FilesReport (Invoke-InWorkspace $wsId (Get-WriteFilesScript $volume $token) 'writing the files')
        Expect ($wrote.Token -eq $token -and $wrote.Sha256) "writing the files in $wsId printed no token or checksum"
        Write-Host "wrote $volume/upgrade-check (random.bin sha256 $($wrote.Sha256))"
        Invoke-UpgradeState @('before', $StateDir, $wsId) 'before'
        Stop-Workspace $conn $wsId $name
    } finally {
        Stop-Installed
    }
    if ($PlantBreakage) { Invoke-UpgradeState @('plant', $PlantBreakage) 'plant' }

    Step 'upgrade to this build'
    Invoke-Msiexec @('/i', "`"$Upgrade`"") 'msi-upgrade.log'
    $second = Assert-InstalledLayout
    Assert-NewerVersion $first.Version $second.Version
    Write-Host "upgraded $($first.Version) to $($second.Version)"
    Assert-InstalledWorks $Policy

    Step 'the newer build lists the workspace, starts it, and its files are there'
    $conn = Start-Installed
    try {
        $w = Get-WorkspaceNow $conn $wsId
        Expect ($null -ne $w) "the upgraded program does not list workspace ${wsId}: $((Invoke-PuddleApi $conn 'Get' '/api/workspaces') | ConvertTo-Json -Depth 4 -Compress)"
        Expect ($w.name -eq $name) "workspace $wsId is called '$($w.name)' after the upgrade, not '$name'"
        Expect ($w.status -eq 'stopped') "workspace $wsId is '$($w.status)' after the upgrade, not stopped"
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
        Write-Host "workspace $name ($wsId) running $([math]::Round($clock.Elapsed.TotalSeconds)) s after the start"
        $read = Read-FilesReport (Invoke-InWorkspace $wsId (Get-ReadFilesScript '/workspace') 'reading the files')
        Expect ($read.Token -eq $token) "the token file in $wsId changed or is gone after the upgrade: '$($read.Token)', expected $token"
        Expect ($read.Sha256 -eq $wrote.Sha256) "random.bin in $wsId changed after the upgrade: sha256 $($read.Sha256), was $($wrote.Sha256)"
        Write-Host "the files in $wsId are intact after the upgrade"
        Invoke-UpgradeState @('after', $StateDir, $wsId) 'after'
        Stop-Workspace $conn $wsId $name
        Remove-Workspace $conn $wsId $name
    } finally {
        Stop-Installed
    }

    Invoke-Msiexec @('/x', "`"$Upgrade`"") 'msi-uninstall.log'
    Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the uninstall"
}

# ---- The installer as a person meets it: its dialogs, the Start menu shortcut, a launch at sign-in, an uninstall ----

# Win32 calls the dialog and window checks need: a click that does not block on a dialog that opens
# a modal one (UI Automation's Invoke waits for it), and the visible windows of a process.
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class PuddleWin {
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int index);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool IsWindowEnabled(IntPtr h);
  delegate bool EnumProc(IntPtr h, IntPtr l);
  // Every visible top-level window of the processes (or of a MsiDialog class) with its child controls:
  // each row is "top handle|top class|top title|child handle|child class|child text|child enabled".
  // Windows Installer draws plain Win32 controls, so this reads them when UI Automation shows none.
  public static string[] DialogControls(uint[] pids) {
    var rows = new List<string>();
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      var cls = new StringBuilder(256); GetClassName(h, cls, 256);
      if (Array.IndexOf(pids, owner) < 0 && !cls.ToString().StartsWith("MsiDialog")) return true;
      if (!IsWindowVisible(h)) return true;
      var title = new StringBuilder(256); GetWindowText(h, title, 256);
      EnumChildWindows(h, (c, l2) => {
        var cc = new StringBuilder(256); GetClassName(c, cc, 256);
        var ct = new StringBuilder(1024); GetWindowText(c, ct, 1024);
        rows.Add(h.ToInt64() + "|" + cls + "|" + title + "|" + c.ToInt64() + "|" + cc + "|" + ct.ToString().Replace("\r", " ").Replace("\n", " ") + "|" + IsWindowEnabled(c));
        return true;
      }, IntPtr.Zero);
      return true;
    }, IntPtr.Zero);
    return rows.ToArray();
  }
  // The titles of the process's visible top-level windows that have a title bar: what a person sees
  // of the program besides its tray icon.
  public static string[] VisibleWindows(uint pid) {
    var titles = new List<string>();
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner != pid || !IsWindowVisible(h)) return true;
      if ((GetWindowLong(h, -16) & 0x00C00000) != 0x00C00000) return true;
      var title = new StringBuilder(256); GetWindowText(h, title, 256);
      titles.Add(title.ToString());
      return true;
    }, IntPtr.Zero);
    return titles.ToArray();
  }
}
'@

# What the installer's buttons are pressed in, first match wins: a person clicks through Next, Install and
# Finish, and dismisses a message with OK. Cancel and Back are never pressed.
$DialogButtonOrder = @('^&?Install$', '^&?Next\b', '^&?Finish$', '^&?OK$', '^&?Close$')

# The enabled button to press on a dialog, or $null when there is none (the progress page has only Cancel).
function Select-DialogButton($Buttons) {
    foreach ($pattern in $DialogButtonOrder) {
        $hit = @($Buttons | Where-Object { $_.Enabled -and $_.Name -match $pattern }) | Select-Object -First 1
        if ($hit) { return $hit }
    }
    $null
}

# The windows msiexec shows: title, the text of every label, every button. UI Automation reads the
# real window, so this is what the person reads.
function Get-InstallerDialogs {
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
    $A = [System.Windows.Automation.AutomationElement]
    $ids = @(Get-Process -Name msiexec -ErrorAction SilentlyContinue | ForEach-Object Id)
    $isText = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
    $isButton = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    foreach ($w in $A::RootElement.FindAll('Children', [System.Windows.Automation.Condition]::TrueCondition)) {
        try {
            if ($ids -notcontains $w.Current.ProcessId -and $w.Current.ClassName -notlike 'MsiDialog*') { continue }
            $texts = @($w.FindAll('Descendants', $isText) | ForEach-Object { $_.Current.Name } | Where-Object { $_ })
            $buttons = @($w.FindAll('Descendants', $isButton) | ForEach-Object {
                [pscustomobject]@{ Name = $_.Current.Name; Enabled = $_.Current.IsEnabled; Element = $_ } })
            if ($texts.Count -eq 0 -and $buttons.Count -eq 0) { continue }
            [pscustomobject]@{ Title = $w.Current.Name; Texts = $texts; Buttons = $buttons }
        } catch { } # a dialog that closed while it was read
    }
}

# The dialogs as Win32 controls: Static text and Button children of the installer's windows.
function Get-InstallerDialogsWin32 {
    $ids = [uint32[]]@(Get-Process -Name msiexec -ErrorAction SilentlyContinue | ForEach-Object Id)
    $rows = foreach ($r in [PuddleWin]::DialogControls($ids)) {
        $f = $r -split '\|', 7
        [pscustomobject]@{ Top = $f[0]; Title = $f[2]; Handle = [int64]$f[3]; Class = $f[4]; Text = $f[5]; Enabled = ($f[6] -eq 'True') }
    }
    foreach ($g in @($rows) | Group-Object Top) {
        $texts = @($g.Group | Where-Object { $_.Class -eq 'Static' -and $_.Text.Trim() } | ForEach-Object { $_.Text.Trim() })
        $buttons = @($g.Group | Where-Object { $_.Class -eq 'Button' -and $_.Text.Trim() } | ForEach-Object {
            [pscustomobject]@{ Name = $_.Text.Trim(); Enabled = $_.Enabled; Element = $null; Handle = $_.Handle } })
        if ($texts.Count -eq 0 -and $buttons.Count -eq 0) { continue }
        [pscustomobject]@{ Title = $g.Group[0].Title; Texts = $texts; Buttons = $buttons }
    }
}

function Invoke-DialogButton($Button) {
    if (-not $Button.Element) { [void][PuddleWin]::PostMessage([IntPtr]$Button.Handle, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero); return }
    $handle = $Button.Element.Current.NativeWindowHandle
    if ($handle -ne 0) { [void][PuddleWin]::PostMessage([IntPtr]$handle, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero); return } # BM_CLICK
    $Button.Element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}

# Runs msiexec with the installer's own dialogs and clicks through them, as a person does: logs each
# new dialog (title, text, buttons) to <LogName>-dialogs.log, presses its button, and returns the
# exit code, the seconds the whole run took and every dialog seen. $OnDialog (a scriptblock) sees
# each new dialog before its button is pressed.
function Invoke-MsiexecWithUi([string[]]$Arguments, [string]$LogName, [int]$TimeoutSec = 300, [scriptblock]$OnDialog = $null) {
    $log = Join-Path $LogDir $LogName
    $dialogLog = Join-Path $LogDir ($LogName -replace '\.log$', '-dialogs.log')
    Set-Content -LiteralPath $dialogLog -Value @()
    Step "msiexec $($Arguments -join ' ') with the installer's dialogs  (log: $log)"
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList ($Arguments + @('/norestart', '/l*v', "`"$log`"")) -PassThru
    $seen = [ordered]@{}
    $dumped = $false
    while (-not $p.HasExited) {
        if ($clock.Elapsed.TotalSeconds -gt $TimeoutSec) {
            Stop-Process -Id $p.Id -Force
            Fail "msiexec $($Arguments -join ' ') was still running after $TimeoutSec s; dialogs so far: $(($seen.Keys) -join ' || ')"
        }
        $found = @(Get-InstallerDialogs)
        if ($found.Count -eq 0) { $found = @(Get-InstallerDialogsWin32) }
        foreach ($d in $found) {
            $key = "$($d.Title) | $($d.Texts -join ' / ')"
            if ($seen.Contains($key)) { continue }
            $seen[$key] = $d
            $line = "[{0:N1} s] dialog '{1}': {2} || buttons: {3}" -f $clock.Elapsed.TotalSeconds, $d.Title, ($d.Texts -join ' / '),
                (($d.Buttons | ForEach-Object { if ($_.Enabled) { $_.Name } else { "($($_.Name))" } }) -join ', ')
            Write-Host $line
            Add-Content -LiteralPath $dialogLog -Value $line
            if ($OnDialog) { & $OnDialog $d }
            $button = Select-DialogButton $d.Buttons
            if ($button) {
                Invoke-DialogButton $button
                Add-Content -LiteralPath $dialogLog -Value "           pressed '$($button.Name)'"
            }
        }
        if ($seen.Count -eq 0 -and -not $dumped -and $clock.Elapsed.TotalSeconds -gt 20) {
            # No dialog read yet: say what UI Automation does see, for the diagnosis.
            $dumped = $true
            Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
            $top = [System.Windows.Automation.AutomationElement]::RootElement.FindAll('Children', [System.Windows.Automation.Condition]::TrueCondition)
            $rows = @($top | ForEach-Object { "$($_.Current.ProcessId) '$($_.Current.ClassName)' '$($_.Current.Name)'" })
            $note = "no dialog read after 20 s; msiexec pids: $((Get-Process -Name msiexec -ErrorAction SilentlyContinue | ForEach-Object Id) -join ','); top-level windows: $($rows -join ' ; ')"
            $controls = @([PuddleWin]::DialogControls([uint32[]]@(Get-Process -Name msiexec -ErrorAction SilentlyContinue | ForEach-Object Id)))
            $note += "; win32 controls: $($controls -join ' ;; ')"
            Write-Host $note
            Add-Content -LiteralPath $dialogLog -Value $note
        }
        Start-Sleep -Milliseconds 250
    }
    $p.WaitForExit()
    [pscustomobject]@{ Code = $p.ExitCode; Seconds = $clock.Elapsed.TotalSeconds; Dialogs = @($seen.Values); Log = $log }
}

# What is wrong with the dialogs a run showed: too few, one with no text, a raw "[1]" placeholder.
function Find-DialogProblems($Dialogs) {
    # The pages of WixUI_InstallDir without the licence page: welcome, folder, ready, progress, finish.
    if (@($Dialogs).Count -lt 4) { "only $(@($Dialogs).Count) dialogs were seen; a person sees at least four (welcome, folder, ready, finish)" }
    foreach ($d in $Dialogs) {
        if ($d.Texts.Count -eq 0) { "the dialog '$($d.Title)' shows no text" }
        foreach ($t in $d.Texts) {
            # Windows Installer's own transient "Generating script operations for action: [1]" line is not
            # in the package (no ActionText row can change it) and lasts about a second; skip it.
            if ($t -notmatch '^Generating script operations for action' -and $t -match '\[[0-9A-Za-z_%#!$]*\]') { "the dialog '$($d.Title)' shows a raw placeholder: $t" }
        }
    }
    if (-not (@($Dialogs) | Where-Object { ($_.Texts -join ' ') -match 'Puddle' })) { 'no dialog names Puddle' }
}

function Test-RefusalShown($Dialogs) {
    [bool](@($Dialogs) | Where-Object { (($_.Texts -join ' ') -replace '\s+', ' ').Contains($RefusalText) })
}

# The first install as a person makes it: the installer's dialogs, Next to Finish, under a minute.
function Install-WithUi([string]$Package, [string]$LogName, [string]$Plant) {
    $slow = { param($d) if ($Plant -eq 'slow-install' -and (@($d.Buttons) | Where-Object { $_.Name -match '^&?Install$' })) { Start-Sleep -Seconds 65 } }
    $ui = Invoke-MsiexecWithUi @('/i', "`"$Package`"") $LogName 300 $slow
    Expect ($ui.Code -eq 0) "msiexec /i exited with $($ui.Code); see $($ui.Log)"
    $problems = @(Find-DialogProblems $ui.Dialogs)
    Expect ($problems.Count -eq 0) "the installer's dialogs: $($problems -join '; ')"
    Expect ($ui.Seconds -lt $InstallBudgetSec) ("the install with the dialogs took {0:N0} s, over the {1} s bar" -f $ui.Seconds, $InstallBudgetSec)
    Write-Host ("installed through {0} dialogs in {1:N0} s" -f $ui.Dialogs.Count, $ui.Seconds)
    # The Finish page's "Launch Puddle" box is ticked, so the program may be running now; the test starts its own.
    Stop-Installed
}
$InstallBudgetSec = 60

# The upgrade over a running Puddle as a person meets it: the refusal is in a dialog the screen reader
# (UI Automation) reads, and nothing is changed.
function Assert-UpgradeRefusalDialog([string]$UpgradeMsi, [string]$FirstVersion) {
    $digest = Get-TreeDigest
    $ui = Invoke-MsiexecWithUi @('/i', "`"$UpgradeMsi`"") 'msi-upgrade-refused-ui.log' 180
    Expect ($ui.Code -ne 0) 'the interactive upgrade went ahead with the program running; it must refuse'
    Expect (Test-RefusalShown $ui.Dialogs) "no dialog showed '$RefusalText'; the dialogs: $(($ui.Dialogs | ForEach-Object { "'$($_.Title)': $($_.Texts -join ' / ')" }) -join ' || ')"
    Expect (((Get-TreeDigest) -join "`n") -eq ($digest -join "`n")) 'the refused interactive upgrade changed the install folder'
    $entries = @(Get-UninstallEntries)
    Expect ($entries.Count -eq 1 -and $entries[0].Version -eq $FirstVersion) "the refused interactive upgrade changed the installed version: $(($entries | ForEach-Object Version) -join ', ')"
    Write-Host "the refusal dialog read: $RefusalText"
}

$StartMenuLink = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$ProductName\$ProductName.lnk"

# Whether a shortcut's target is the installed program; the problem as text, or $null.
function Find-ShortcutProblem([string]$Target, [string]$Expected) {
    if ([string]::IsNullOrEmpty($Target)) { return 'the Start menu shortcut has no target' }
    if ($Target -ne $Expected) { return "the Start menu shortcut opens $Target, not $Expected" }
    $null
}

function Assert-StartMenuShortcut {
    Expect (Test-Path -LiteralPath $StartMenuLink) "the Start menu has no $StartMenuLink"
    $target = (New-Object -ComObject WScript.Shell).CreateShortcut($StartMenuLink).TargetPath
    $problem = Find-ShortcutProblem $target (Join-Path $InstallDir 'puddle-app.exe')
    Expect ($null -eq $problem) $problem
}

# A person opens Puddle from the Start menu: the shortcut starts the program, a window with a title
# bar is visible and shows "Welcome to Puddle" (read through UI Automation).
function Test-ShortcutOpensWelcome {
    Stop-Installed
    Start-Process -FilePath $StartMenuLink
    $app = Wait-Until 'the Start menu shortcut started puddle-app.exe' 60 {
        Get-Process -Name 'puddle-app' -ErrorAction SilentlyContinue | Select-Object -First 1
    }
    $appId = $app.Id
    try {
        $titles = Wait-Until "puddle-app ($appId), started from the shortcut, shows no window" 60 { @([PuddleWin]::VisibleWindows($appId)) }
        Write-Host "the shortcut opened a window titled '$($titles -join "', '")'"
        $script = Join-Path $PSScriptRoot '..\crates\puddle-app\e2e\welcome-window.mjs'
        $lines = @(& node $script $appId 2>&1 | ForEach-Object { "$_" })
        $code = $LASTEXITCODE
        $lines | ForEach-Object { Write-Host $_ }
        $lines | Set-Content -LiteralPath (Join-Path $LogDir 'shortcut-welcome.log')
        Expect ($code -eq 0) "the window opened from the Start menu shortcut does not show 'Welcome to Puddle': $(($lines | Where-Object { $_ -match '^FAIL' }) -join ' | ')"
    } finally {
        Stop-Installed
    }
}

# The command of the sign-in entry (puddle-app.exe --at-login): the tray icon is there and no window is
# visible. -Plant 'visible-at-login' starts it without the flag, as a program that ignored it would behave.
function Test-AtLoginLaunch([string]$Plant) {
    Stop-Installed
    $flags = if ($Plant -eq 'visible-at-login') { @() } else { @('--at-login') }
    [void](Start-Installed $flags)
    $appId = $script:StartedAppPid
    try {
        Wait-Until 'the program started at sign-in shows no tray icon' 60 { @(Get-PuddleTrayIcons).Count -ge 1 } -PollMs 1000 | Out-Null
        $titles = @([PuddleWin]::VisibleWindows($appId))
        Expect ($titles.Count -eq 0) "the program started with --at-login shows a window: '$($titles -join "', '")'"
        Write-Host 'the sign-in launch has a tray icon and no visible window'
    } finally {
        Stop-Installed
    }
}

# Every file of the data folder, by relative path and size (the connection file is rewritten by each start).
function Get-DataListing {
    @(Get-ChildItem -LiteralPath $DataDir -Recurse -Force -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -ne 'api.json' } |
        ForEach-Object { "$($_.FullName.Substring($DataDir.Length)) $($_.Length)" } | Sort-Object)
}

# The uninstall a person does with a stopped workspace that holds files: the program goes, the data
# folder and the workspace's volume stay, and after a reinstall the same workspace id is listed and its
# files are there byte for byte. $Package is the installed build (reinstalled, then removed again).
function Test-UninstallKeepsData([string]$Package, [string]$Plant) {
    Step 'the uninstall keeps the data folder and the volume; a reinstall lists the same workspace with its files'
    Invoke-Msiexec @('/i', "`"$Package`"") 'msi-keep-install.log'
    $name = 'keep-test'
    $token = [guid]::NewGuid().ToString()
    $conn = Start-Installed
    try {
        $wsId = New-Workspace $conn $name
        Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
        $volume = "/workspace"  # the single guest folder every workspace mounts its volume at
        $wrote = Read-FilesReport (Invoke-InWorkspace $wsId (Get-WriteFilesScript $volume $token) 'writing the files')
        Expect ($wrote.Token -eq $token -and $wrote.Sha256) "writing the files in $wsId printed no token or checksum"
        Stop-Workspace $conn $wsId $name
    } finally {
        Stop-Installed
    }
    $before = @(Get-DataListing)
    $ofWorkspace = @($before | Where-Object { $_.Contains($wsId) })
    Expect ($ofWorkspace.Count -gt 0) "no file in the data folder carries the workspace id $wsId, so the check that its volume stays would prove nothing"

    Invoke-Msiexec @('/x', "`"$Package`"") 'msi-keep-uninstall.log'
    Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the uninstall"
    if ($Plant -eq 'data-lost') {
        # An uninstaller that took the volume with it.
        $victim = Get-ChildItem -LiteralPath $DataDir -Recurse -Force -File | Where-Object { $_.FullName.Contains($wsId) } | Sort-Object Length -Descending | Select-Object -First 1
        Remove-Item -LiteralPath $victim.FullName -Force
    }
    Expect (Test-Path -LiteralPath $DataDir) "the uninstall removed the data folder $DataDir"
    $after = @(Get-DataListing)
    $lost = @($before | Where-Object { $after -notcontains $_ })
    Expect ($lost.Count -eq 0) "the uninstall removed or changed $($lost.Count) data files, e.g. $(($lost | Select-Object -First 3) -join '; ')"
    Write-Host "the uninstall kept the data folder: $($after.Count) files, $($ofWorkspace.Count) of workspace $wsId"

    Invoke-Msiexec @('/i', "`"$Package`"") 'msi-keep-reinstall.log'
    $conn = Start-Installed
    try {
        $w = Get-WorkspaceNow $conn $wsId
        Expect ($null -ne $w) "the reinstalled program does not list workspace ${wsId}: $((Invoke-PuddleApi $conn 'Get' '/api/workspaces') | ConvertTo-Json -Depth 4 -Compress)"
        Expect ($w.name -eq $name) "workspace $wsId is called '$($w.name)' after the reinstall, not '$name'"
        Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
        $read = Read-FilesReport (Invoke-InWorkspace $wsId (Get-ReadFilesScript $volume) 'reading the files')
        Expect ($read.Token -eq $token) "the token file in $wsId changed or is gone after the reinstall: '$($read.Token)', expected $token"
        Expect ($read.Sha256 -eq $wrote.Sha256) "random.bin in $wsId changed after the reinstall: sha256 $($read.Sha256), was $($wrote.Sha256)"
        Write-Host "workspace $wsId is listed again and its files are byte for byte the same"
        Stop-Workspace $conn $wsId $name
        Remove-Workspace $conn $wsId $name
    } finally {
        Stop-Installed
    }
    Invoke-Msiexec @('/x', "`"$Package`"") 'msi-keep-final-uninstall.log'
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
    Check 'a raw template is found' (@(Find-PlaceholderRows @([pscustomobject]@{ Action = 'InstallFiles'; Description = 'Copying new files'; Template = 'File: [1],  Directory: [9],  Size: [6]' })).Count -eq 1)
    Check 'a placeholder in the text is found' (@(Find-PlaceholderRows @([pscustomobject]@{ Action = 'X'; Description = 'Folder: [1]'; Template = '' })).Count -eq 1)
    Check 'plain text with no template passes' (@(Find-PlaceholderRows @([pscustomobject]@{ Action = 'InstallFiles'; Description = "Copying Puddle's files"; Template = '' })).Count -eq 0)
    Check 'a package with no text is named' (@(Find-InstallerTextProblems @()).Count -eq $ActionsWithText.Count)
    Check 'a package with every text and no template passes' (@(Find-InstallerTextProblems @($ActionsWithText | ForEach-Object { [pscustomobject]@{ Action = $_; Description = 'Plain'; Template = '' } })).Count -eq 0)
    Check 'Wait-Until returns what the probe found' ((Wait-Until 'x' 5 { 'found' } 10) -eq 'found')
    $calls = 0
    Check 'Wait-Until polls until the probe succeeds' ((Wait-Until 'x' 5 { $script:calls++; $script:calls -ge 3 } 10) -eq $true -and $calls -eq 3)
    Check 'Wait-Until gives up and says what it waited for' (Throws { Wait-Until 'the thing' 1 { $false } 100 } 'the thing \(gave up after 1 s\)')
    $sum = 'a' * 64
    $files = Read-FilesReport "f81d4fae-7dec-11d0-a765-00a0c91e6bf6`r`n$sum  /workspaces/w1/upgrade-check/random.bin`n"
    Check 'the files report gives the token' ($files.Token -eq 'f81d4fae-7dec-11d0-a765-00a0c91e6bf6')
    Check 'the files report gives the checksum' ($files.Sha256 -eq $sum)
    $none = Read-FilesReport "cat: /workspaces/w1/upgrade-check/token.txt: No such file or directory`n"
    Check 'a files report without them gives neither' ($null -eq $none.Token -and $null -eq $none.Sha256)
    $write = Get-WriteFilesScript '/workspaces/w1' 'tok'
    Check 'the write script refuses a folder that is not a mount' ($write -match [regex]::Escape("grep -q ' /workspaces/w1 ' /proc/mounts"))
    Check 'the write script writes where the read script reads' (($write -match [regex]::Escape('/workspaces/w1/upgrade-check/random.bin')) -and ((Get-ReadFilesScript '/workspaces/w1') -match [regex]::Escape('sha256sum /workspaces/w1/upgrade-check/random.bin')))
    # Wait-WorkspaceRunning against a fake API: made and down twice, then starting, then running.
    $script:seen = @(
        [pscustomobject]@{ status = 'created'; busy = $false }, [pscustomobject]@{ status = 'created'; busy = $false },
        [pscustomobject]@{ status = 'starting'; busy = $true }, [pscustomobject]@{ status = 'running'; busy = $false; image = 'debian' })
    $script:polls = 0
    $script:starts = @()
    function Get-WorkspaceNow($Conn, [string]$Id) { $script:seen[[math]::Min($script:polls++, $script:seen.Count - 1)] }
    function Invoke-PuddleApi($Conn, [string]$Method, [string]$Path) { $script:starts += "$Method $Path" }
    $ran = Wait-WorkspaceRunning $null 'w1' 'test' 10
    Check 'a workspace made but down is started once and waited for' ($ran.status -eq 'running' -and ($script:starts -join ',') -eq 'Post /api/workspaces/w1/start')
    $script:seen = @([pscustomobject]@{ status = 'volume_missing'; busy = $false })
    $script:polls = 0
    Check 'a workspace without its volume fails at once' (Throws { Wait-WorkspaceRunning $null 'w1' 'test' 10 } 'test \(w1\) is volume_missing')
    Check 'a newer package version passes' (-not (Throws { Assert-NewerVersion '0.0.0' '0.0.1' } '.'))
    Check 'the same version is not an upgrade' (Throws { Assert-NewerVersion '0.2.0' '0.2.0' } 'not newer')
    Check 'versions compare as numbers' (Throws { Assert-NewerVersion '0.10.0' '0.9.1' } 'not newer')
    # The installer's dialogs: which button a person presses, and what is wrong with what they read.
    function Btn([string]$Name, [bool]$Enabled = $true) { [pscustomobject]@{ Name = $Name; Enabled = $Enabled } }
    function Dlg([string]$Title, [string[]]$Texts, $Buttons = @()) { [pscustomobject]@{ Title = $Title; Texts = $Texts; Buttons = $Buttons } }
    Check 'the welcome page gets Next' ((Select-DialogButton @((Btn '&Back'), (Btn '&Next >'), (Btn 'Cancel'))).Name -eq '&Next >')
    Check 'the ready page gets Install' ((Select-DialogButton @((Btn '&Back'), (Btn '&Install'), (Btn 'Cancel'))).Name -eq '&Install')
    Check 'the finish page gets Finish' ((Select-DialogButton @((Btn '&Back' $false), (Btn '&Finish'), (Btn 'Cancel' $false))).Name -eq '&Finish')
    Check 'a message gets OK' ((Select-DialogButton @((Btn 'OK'))).Name -eq 'OK')
    Check 'the progress page gets no button (Cancel is never pressed)' ($null -eq (Select-DialogButton @((Btn '&Back' $false), (Btn '&Next >' $false), (Btn 'Cancel'))))
    $pages = @((Dlg 'Puddle Setup' @('Welcome to the Puddle Setup Wizard')), (Dlg 'Puddle Setup' @('Destination Folder')), (Dlg 'Puddle Setup' @('Ready to install Puddle')), (Dlg 'Puddle Setup' @("Copying Puddle's files")), (Dlg 'Puddle Setup' @('Completed the Puddle Setup Wizard')))
    Check 'a plain installer run has no dialog problem' (@(Find-DialogProblems $pages).Count -eq 0)
    Check 'a run with too few dialogs is named' ((@(Find-DialogProblems $pages[0..1]) -join ';') -match 'only 2 dialogs')
    Check 'a dialog with no text is named' ((@(Find-DialogProblems ($pages + (Dlg 'Puddle Setup' @()))) -join ';') -match 'shows no text')
    Check 'the installer own script-generation line is not a raw placeholder' (@(Find-DialogProblems ($pages + (Dlg 'Puddle Setup' @('Generating script operations for action: [1]')))).Count -eq 0)
    Check 'a raw placeholder in a dialog is named' ((@(Find-DialogProblems ($pages + (Dlg 'Puddle Setup' @('File: [1],  Directory: [9]')))) -join ';') -match 'raw placeholder')
    Check 'a run that never says Puddle is named' ((@(Find-DialogProblems @((Dlg 'Setup' @('a')), (Dlg 'Setup' @('b')), (Dlg 'Setup' @('c')), (Dlg 'Setup' @('d')))) -join ';') -match 'names Puddle')
    Check 'the refusal is found in a message box' (Test-RefusalShown @((Dlg 'Puddle' @("$RefusalText"))))
    Check 'a wrapped refusal is found' (Test-RefusalShown @((Dlg 'Puddle' @($RefusalText.Replace(', then', ",`r`nthen")))))
    Check 'no refusal is found when the upgrade went through' (-not (Test-RefusalShown $pages))
    Check 'a shortcut to the program passes' ($null -eq (Find-ShortcutProblem 'C:\x\Puddle\puddle-app.exe' 'C:\X\Puddle\puddle-app.exe'))
    Check 'a shortcut to another program is named' ((Find-ShortcutProblem 'C:\x\Puddle\puddle.exe' 'C:\x\Puddle\puddle-app.exe') -match 'opens .*puddle.exe, not')
    Check 'a shortcut with no target is named' ((Find-ShortcutProblem '' 'C:\x') -match 'no target')
    Check 'this process sees its own visible windows as none when it has none' (@([PuddleWin]::VisibleWindows(0)).Count -eq 0)
    # The install-folder digest: equal for the same bytes, different when one byte or one file changes.
    $savedInstallDir = $InstallDir
    $InstallDir = Join-Path ([IO.Path]::GetTempPath()) "msi-digest-$([guid]::NewGuid())"
    try {
        New-Item -ItemType Directory -Force -Path (Join-Path $InstallDir 'runtime') | Out-Null
        Set-Content -LiteralPath (Join-Path $InstallDir 'a.exe') -Value 'one'
        Set-Content -LiteralPath (Join-Path $InstallDir 'runtime\b.dll') -Value 'two'
        $d1 = (Get-TreeDigest) -join "`n"
        Check 'the digest is stable' ($d1 -eq ((Get-TreeDigest) -join "`n"))
        Set-Content -LiteralPath (Join-Path $InstallDir 'runtime\b.dll') -Value 'twp'
        Check 'the digest sees one changed byte' ($d1 -ne ((Get-TreeDigest) -join "`n"))
        Set-Content -LiteralPath (Join-Path $InstallDir 'runtime\b.dll') -Value 'two'
        Set-Content -LiteralPath (Join-Path $InstallDir 'c.txt') -Value 'x'
        Check 'the digest sees an added file' ($d1 -ne ((Get-TreeDigest) -join "`n"))
    } finally {
        Remove-Item -LiteralPath $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
        $InstallDir = $savedInstallDir
    }
    if ($failures -gt 0) { Fail "$failures self-test checks failed" }

    # The icon checks: the duck passes, a plain yellow disc (what the installer once shipped) does not.
    $duckIco = (Resolve-Path (Join-Path $PSScriptRoot '..\crates\puddle-app\icons\icon.ico')).Path
    Check 'the duck icon file is the duck' (-not (Throws { Assert-FileIsDuck $duckIco 'the file' } '.'))
    $discIco = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName() + '.ico')
    $disc = New-Object System.Drawing.Bitmap 256, 256
    $g = [System.Drawing.Graphics]::FromImage($disc)
    $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(246, 196, 49))), 12, 12, 232, 232)
    $g.Dispose()
    $stream = [IO.File]::Create($discIco)
    ([System.Drawing.Icon]::FromHandle($disc.GetHicon())).Save($stream)
    $stream.Dispose()
    Check 'a yellow disc is refused as the icon' (Throws { Assert-FileIsDuck $discIco 'the file' } 'is not the duck')
    [IO.File]::Delete($discIco)
    Write-Host 'self-test ok'
    return
}

foreach ($path in @($Msi, $BaselineMsi, $UpgradeMsi) | Where-Object { $_ }) {
    Expect (Test-Path -LiteralPath $path) "no such MSI: $path"
}
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
foreach ($path in @($Msi, $BaselineMsi, $UpgradeMsi) | Where-Object { $_ }) {
    Expect (Test-PerUserPackage $path) "$path is not a per-user package (needs no elevation, no ALLUSERS)"
}
Step 'the package names its icon so the shortcut shows it'
foreach ($path in @($Msi, $UpgradeMsi) | Where-Object { $_ }) { Assert-PackageIcons $path }

Step 'the installer page says what it does in plain words'
# The older build of -BaselineMsi is a release made before this text; only the MSIs of this commit are held to it.
foreach ($path in @($Msi, $UpgradeMsi) | Where-Object { $_ }) {
    $rows = @(Get-ActionTextRows $path)
    $problems = @(Find-InstallerTextProblems $rows)
    Expect ($problems.Count -eq 0) "$path does not say what it does in plain words on the Installing page: $($problems -join '; ')"
    $copy = @($rows | Where-Object { $_.Action -eq 'InstallFiles' })
    Expect ($copy.Count -eq 1 -and $copy[0].Description -match "^Copying Puddle's files") "$path : InstallFiles says '$($copy.Description)', not the plain text"
}

if ($BaselineMsi) {
    Test-UpgradeFromBaseline $BaselineMsi $UpgradeMsi $policy
    Step 'ok'
    return
}

Install-WithUi $Msi 'msi-install.log' $PlantFootprint
$first = Assert-InstalledLayout
if ($PlantFootprint -eq 'no-shortcut') { Remove-Item -LiteralPath $StartMenuLink -Force }
if ($PlantFootprint -eq 'wrong-shortcut-target') {
    $link = (New-Object -ComObject WScript.Shell).CreateShortcut($StartMenuLink)
    $link.TargetPath = Join-Path $InstallDir 'puddle.exe'
    $link.Save()
}
Step 'the Start menu shortcut exists and opens the installed program'
Assert-StartMenuShortcut
Step 'the program and its Start menu shortcut carry the duck icon'
Assert-InstalledIcon
Write-Host "installed version $($first.Version) in $InstallDir"
# The program must report the version of the package it came in (the kept MSI is named by it).
$reported = (& (Join-Path $InstallDir 'puddle.exe') --version) -join ' '
Expect ($reported -match [regex]::Escape($first.Version)) "puddle --version says '$reported' for package version $($first.Version)"
Step 'the Start menu shortcut opens a window on "Welcome to Puddle"'
Test-ShortcutOpensWelcome
Assert-InstalledWorks $policy

if ($policy -eq 'run') {
    Step 'the installed program makes a workspace from the stock image, runs a command in it, stops and deletes it'
    $conn = Start-Installed
    try {
        Test-WorkspaceLifecycle $conn
    } finally {
        Stop-Installed
    }
    Step 'a hard kill of the app (TerminateProcess) ends its VMs within 5 s and loses no commit'
    try {
        Test-HardKill
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

Step 'a launch with the sign-in entry command (--at-login) shows the tray icon and no window'
Test-AtLoginLaunch $PlantFootprint

$current = $Msi
if ($UpgradeMsi) {
    Step 'the upgrade refuses while the program, or a workspace, runs; then it goes through once Puddle is quit'
    $conn = Start-Installed
    try {
        Assert-UpgradeRefused $UpgradeMsi 'with the program running' 'msi-upgrade-refused-app.log' $first.Version
        # What the same refusal looks like to a person: a dialog whose text UI Automation reads.
        # -PlantFootprint no-refusal quits the program first, so the installer has nothing to refuse.
        if ($PlantFootprint -eq 'no-refusal') { Stop-Installed }
        Assert-UpgradeRefusalDialog $UpgradeMsi $first.Version
        if ($policy -eq 'run') {
            $name = 'refusal-test'
            $wsId = New-Workspace $conn $name
            Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
            Expect (@(Get-RuntimeProcesses).Count -gt 0) 'no workspace runtime process runs, so the refusal below would prove nothing'
            Assert-UpgradeRefused $UpgradeMsi 'with a workspace running' 'msi-upgrade-refused-workspace.log' $first.Version
            Expect ($null -ne (Get-WorkspaceNow $conn $wsId) -and (Get-WorkspaceNow $conn $wsId).status -eq 'running') 'the refused upgrade did not leave the workspace running'
            Stop-Workspace $conn $wsId $name
            Remove-Workspace $conn $wsId $name
        }
    } finally {
        # Quitting is the user's act; the test's stand-in for it ends what runs from the install folder.
        Stop-Installed
    }

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

    Step 'the lower version does not install over the higher one, and what the higher one holds stays'
    Assert-DowngradeRefused $Msi $second.Version 'msi-downgrade-refused.log'
    Expect ((Get-Content -LiteralPath $marker -Raw) -eq $markerText) 'the refused downgrade changed the marker in the data folder'
    $dataKept = @(Get-ChildItem -LiteralPath $DataDir -Recurse -Force -File | ForEach-Object { $_.FullName })
    $lostToDowngrade = @($dataAfter | Where-Object { $dataKept -notcontains $_ })
    Expect ($lostToDowngrade.Count -eq 0) "the refused downgrade removed data files: $($lostToDowngrade -join ', ')"
    Assert-InstalledWorks $policy
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
if ($policy -eq 'run') { Test-UninstallKeepsData $current $PlantFootprint }
Step 'ok'
