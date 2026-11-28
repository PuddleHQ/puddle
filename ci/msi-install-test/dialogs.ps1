# SPDX-License-Identifier: AGPL-3.0-or-later
# Dialog phase: the installer as a person meets it (dialogs, Start menu shortcut, sign-in launch, uninstall that keeps data).
# Dot-sourced by ci/msi-install-test.ps1 at script scope: it shares the main script's variables and parameters.

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

# What the installer's buttons are pressed in, first match wins: a person clicks Install on the first page (the
# Default setup), Finish at the end, dismisses a message with OK and confirms the uninstall prompt with Yes. Cancel,
# Back and Custom setup are never pressed here (a run that wants Custom setup passes its own order).
$DialogButtonOrder = @('^&?Install$', '^&?Remove$', '^&?Next\b', '^&?Finish$', '^&?OK$', '^&?Close$', '^&?Yes$')
# The order of a run that takes the Custom setup path: that button first, then Install on its page.
$CustomSetupButtonOrder = @('^&?Custom setup$') + $DialogButtonOrder

# The enabled button to press on a dialog, or $null when there is none (the progress page has only Cancel).
function Select-DialogButton($Buttons, [string[]]$Order = $DialogButtonOrder) {
    foreach ($pattern in $Order) {
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
# -OwnLogOff leaves out /l*v, so the package's own logging (MsiLogging) is what writes the log, as for a person
# who double-clicks the MSI; the run then reports no Log.
function Invoke-MsiexecWithUi([string[]]$Arguments, [string]$LogName, [int]$TimeoutSec = 300, [scriptblock]$OnDialog = $null, [string[]]$ButtonOrder = $DialogButtonOrder, [switch]$OwnLogOff) {
    $log = Join-Path $LogDir $LogName
    $dialogLog = Join-Path $LogDir ($LogName -replace '\.log$', '-dialogs.log')
    Set-Content -LiteralPath $dialogLog -Value @()
    Step "msiexec $($Arguments -join ' ') with the installer's dialogs  (log: $log)"
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $logArguments = if ($OwnLogOff) { @() } else { @('/l*v', "`"$log`"") }
    $p = Start-Process -FilePath 'msiexec.exe' -ArgumentList ($Arguments + @('/norestart') + $logArguments) -PassThru
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
            $button = Select-DialogButton $d.Buttons $ButtonOrder
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
    # The Default setup: the first page, the progress page, the finish page.
    if (@($Dialogs).Count -lt 3) { "only $(@($Dialogs).Count) dialogs were seen; a person sees at least three (first page, installing, finished)" }
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

# What is wrong with the pages of the installer's own wizard (crates/puddle-app/wix/main.wxs): the first page
# offers Install and Custom setup, the Custom setup path shows its page, WixUI's folder and ready pages never
# appear (Default setup has three pages), and the last page says Puddle is installed. $Flow is 'default' or 'custom'.
function Find-WizardProblems($Dialogs, [string]$Flow) {
    $all = @($Dialogs)
    $first = $all | Select-Object -First 1
    $names = if ($first) { @($first.Buttons | ForEach-Object { $_.Name }) } else { @() }
    if (-not ($names -match '^&?Install$') -or -not ($names -match '^&?Custom setup$')) {
        "the first page does not offer Install and Custom setup; its buttons: $($names -join ', ')"
    }
    if (-not ($all | Where-Object { ($_.Texts -join ' ') -match 'Install Puddle' })) { "no page is titled 'Install Puddle'" }
    $custom = @($all | Where-Object { ($_.Texts -join ' ') -match 'Install Puddle in this folder' -or ($_.Texts -join ' ') -match 'Custom setup' -and ($_.Buttons | Where-Object { $_.Name -match '^&?Back$' }) })
    if ($Flow -eq 'custom' -and $custom.Count -eq 0) { 'the Custom setup path never showed its page (folder and options)' }
    if ($Flow -eq 'default' -and $all.Count -gt 0 -and $custom.Count -gt 0) { 'the Default setup showed the Custom setup page' }
    foreach ($d in $all) {
        if (($d.Texts -join ' ') -match 'Destination Folder|Ready to install') { "WixUI's own page '$(($d.Texts -join ' '))' is shown; the wizard has its own pages" }
    }
    if (-not ($all | Where-Object { ($_.Texts -join ' ') -match 'Puddle is installed' })) { "no page says 'Puddle is installed'" }
}

function Test-RefusalShown($Dialogs) {
    [bool](@($Dialogs) | Where-Object { (($_.Texts -join ' ') -replace '\s+', ' ').Contains($RefusalText) })
}

# The first install as a person makes it: the installer's dialogs, Next to Finish, under a minute.
function Install-WithUi([string]$Package, [string]$LogName, [string]$Plant) {
    $slow = { param($d) if ($Plant -eq 'slow-install' -and (@($d.Buttons) | Where-Object { $_.Name -match '^&?Install$' })) { Start-Sleep -Seconds 65 } }
    $ui = Invoke-MsiexecWithUi @('/i', "`"$Package`"") $LogName 300 $slow
    Expect ($ui.Code -eq 0) "msiexec /i exited with $($ui.Code); see $($ui.Log)"
    $problems = @(Find-DialogProblems $ui.Dialogs) + @(Find-WizardProblems $ui.Dialogs 'default')
    Expect ($problems.Count -eq 0) "the installer's dialogs: $($problems -join '; ')"
    Expect ($ui.Seconds -lt $InstallBudgetSec) ("the install with the dialogs took {0:N0} s, over the {1} s bar" -f $ui.Seconds, $InstallBudgetSec)
    Write-Host ("installed through {0} dialogs in {1:N0} s" -f $ui.Dialogs.Count, $ui.Seconds)
    # The Finish page's "Launch Puddle" box is ticked, so the program may be running now; the test starts its own.
    Stop-Installed
}
$InstallBudgetSec = 60

# The Custom setup path, and the setup log, the way a person who double-clicks the MSI meets them: the package
# is removed (silently), then installed through its dialogs with Custom setup, with no /l on the command line,
# so Windows Installer's own log (the package turns it on) is the only one. The finish page must name that log
# (a file under the Temp folder), and the install must work as the Default setup's does. The package stays installed.
function Test-CustomSetupAndLog([string]$Package) {
    Step 'the Custom setup path installs, and the finish page names the setup log'
    Invoke-Msiexec @('/x', "`"$Package`"") 'msi-custom-setup-remove.log'
    $started = Get-Date
    $ui = Invoke-MsiexecWithUi @('/i', "`"$Package`"") 'msi-custom-setup.log' 300 $null $CustomSetupButtonOrder -OwnLogOff
    Expect ($ui.Code -eq 0) "msiexec /i with Custom setup exited with $($ui.Code)"
    $problems = @(Find-DialogProblems $ui.Dialogs) + @(Find-WizardProblems $ui.Dialogs 'custom')
    Expect ($problems.Count -eq 0) "the installer's dialogs on the Custom setup path: $($problems -join '; ')"
    $temp = [System.IO.Path]::GetFullPath($env:TEMP)
    $logs = @(Get-ChildItem -LiteralPath $temp -Filter 'MSI*.LOG' -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -ge $started.AddSeconds(-5) } | Sort-Object LastWriteTime)
    Expect ($logs.Count -ge 1) "the run, started without /l, left no MSI*.LOG in $temp (the package's MsiLogging)"
    $finish = @($ui.Dialogs | Where-Object { ($_.Texts -join ' ') -match 'Puddle is installed' }) | Select-Object -Last 1
    $named = @($logs | Where-Object { ($finish.Texts -join ' ').Contains($_.FullName) })
    Expect ($named.Count -ge 1) "the finish page does not name the setup log; it reads: $($finish.Texts -join ' / '); logs found: $(($logs | ForEach-Object FullName) -join ', ')"
    Expect (@($finish.Buttons | Where-Object { $_.Name -match 'Open the &?log' }).Count -eq 1) "the finish page has no 'Open the log' button; its buttons: $(($finish.Buttons | ForEach-Object Name) -join ', ')"
    Copy-Item -LiteralPath $named[0].FullName -Destination (Join-Path $LogDir 'msi-custom-setup-package.log') -Force
    Write-Host "the finish page names the setup log $($named[0].FullName)"
    Stop-Installed
}

# The upgrade over a running Puddle as a person meets it: the first dialog is the refusal (before the
# wizard's first page, so no Install button to press), the text is read by UI Automation, the run ends there (no
# "setup was interrupted" or failure page after it) and nothing is changed.
function Assert-UpgradeRefusalDialog([string]$UpgradeMsi, [string]$FirstVersion) {
    $digest = Get-TreeDigest
    $ui = Invoke-MsiexecWithUi @('/i', "`"$UpgradeMsi`"") 'msi-upgrade-refused-ui.log' 180
    Expect ($ui.Code -ne 0) 'the interactive upgrade went ahead with the program running; it must refuse'
    $dialogs = @($ui.Dialogs)
    $shown = ($dialogs | ForEach-Object { "'$($_.Title)': $($_.Texts -join ' / ')" }) -join ' || '
    Expect ($dialogs.Count -ge 1 -and (Test-RefusalShown @($dialogs[0]))) "the first dialog did not show '$RefusalText'; the dialogs: $shown"
    Expect (-not (@($dialogs[0].Buttons) | Where-Object { $_.Name -match '^&?(Install|Custom setup)$' })) "the first dialog offers Install; the refusal must come before the wizard's first page: $shown"
    Expect ($dialogs.Count -eq 1) "the refusal is not the last page; the dialogs: $shown"
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
        $script = Join-Path $PSScriptRoot '..\..\crates\puddle-app\e2e\welcome-window.mjs'
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
