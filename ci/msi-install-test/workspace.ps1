# SPDX-License-Identifier: AGPL-3.0-or-later
# Workspace phase: tray icons, the hard kill, the workspace lifecycle through the installed program's API.
# Dot-sourced by ci/msi-install-test.ps1 at script scope: it shares the main script's variables and parameters.

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
    # Windows keeps a killed process in the list until its last handle closes, so the app can still
    # show up for a moment after TerminateProcess returned; give it a few seconds to leave.
    Wait-Until 'a puddle-app process is left after the kill' 10 {
        @(Get-Process -Name 'puddle-app' -ErrorAction SilentlyContinue).Count -eq 0
    } -PollMs 200 | Out-Null
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

# Writes $Token to the sandbox's root disk, outside the volume: into the session user's home folder
# (/home/agent, or /root for a root session) and below /usr/local, where an installed package or
# toolchain lands. An upgrade that makes the sandbox again loses both (QA 11 F1).
function Get-WriteRootDiskScript([string]$Token) {
    "set -e; home=/home/agent; [ -d `$home ] || home=/root; " +
    "echo $Token > `$home/upgrade-check.txt; mkdir -p /usr/local/share/upgrade-check; " +
    "echo $Token > /usr/local/share/upgrade-check/token.txt; sync; " +
    "cat `$home/upgrade-check.txt /usr/local/share/upgrade-check/token.txt"
}

# Reads back what Get-WriteRootDiskScript wrote: one line each, or MISSING-HOME / MISSING-USR.
function Get-ReadRootDiskScript {
    "home=/home/agent; [ -d `$home ] || home=/root; " +
    "cat `$home/upgrade-check.txt 2>/dev/null || echo MISSING-HOME; " +
    "cat /usr/local/share/upgrade-check/token.txt 2>/dev/null || echo MISSING-USR"
}

# The token and the SHA-256 from the output of either script, or $null for what is missing.
function Read-FilesReport([string]$Output) {
    $lines = @($Output -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })
    $sha = $lines | Where-Object { $_ -match '^([0-9a-f]{64})\s' } | ForEach-Object { $Matches[1] } | Select-Object -First 1
    $token = $lines | Where-Object { $_ -match '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' } | Select-Object -First 1
    [pscustomobject]@{ Token = $token; Sha256 = $sha }
}
