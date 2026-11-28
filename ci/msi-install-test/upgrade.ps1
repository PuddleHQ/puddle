# SPDX-License-Identifier: AGPL-3.0-or-later
# Upgrade phase: an older build to this one, with a workspace that holds files.
# Dot-sourced by ci/msi-install-test.ps1 at script scope: it shares the main script's variables and parameters.

# Fails unless the package version $To is newer than $From: Windows Installer only upgrades to a
# higher version, so anything else would test a downgrade or a reinstall.
function Assert-NewerVersion([string]$From, [string]$To) {
    Expect ([version]$To -gt [version]$From) "the upgrade goes from $From to $To, which is not newer"
}

# Runs crates/puddle-app/e2e/upgrade-state.mjs (the state an upgrade must keep) against the running
# installed program; fails when any of its checks fails. Its lines are the job's log.
function Invoke-UpgradeState([string[]]$Arguments, [string]$Phase) {
    $script = Join-Path $PSScriptRoot '..\..\crates\puddle-app\e2e\upgrade-state.mjs'
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
        # A baseline from before the single guest folder (develop before 8110c2c3c, 2026-11-25)
        # mounts the volume at /workspaces/<id>; this build keeps that sandbox and binds the volume
        # at /workspace too, so the files are read back there.
        $where = Invoke-InWorkspace $wsId "if grep -q ' /workspace ' /proc/mounts; then echo /workspace; else echo /workspaces/$wsId; fi" 'finding the volume'
        $volume = @($where | ForEach-Object { $_ -split "`n" } | ForEach-Object { $_.Trim() } | Where-Object { $_ })[-1]
        $wrote = Read-FilesReport (Invoke-InWorkspace $wsId (Get-WriteFilesScript $volume $token) 'writing the files')
        Expect ($wrote.Token -eq $token -and $wrote.Sha256) "writing the files in $wsId printed no token or checksum"
        Write-Host "wrote $volume/upgrade-check (random.bin sha256 $($wrote.Sha256))"
        $rootWrote = @((Invoke-InWorkspace $wsId (Get-WriteRootDiskScript $token) 'writing to the root disk') -split "`n" | Where-Object { $_.Trim() -eq $token })
        Expect ($rootWrote.Count -eq 2) "writing the token to the root disk of $wsId printed it $($rootWrote.Count) times, not 2"
        Write-Host "wrote the token to the root disk (the home folder and /usr/local/share)"
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
        $rootRead = @((Invoke-InWorkspace $wsId (Get-ReadRootDiskScript) 'reading the root disk') -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ })
        Expect (@($rootRead | Where-Object { $_ -eq $token }).Count -eq 2) "the root disk of $wsId lost what the older build wrote to it (the home folder or /usr/local), so the upgrade made the sandbox again: $($rootRead -join ' | ')"
        Write-Host "the root disk of $wsId is intact after the upgrade (home folder and /usr/local)"
        Invoke-UpgradeState @('after', $StateDir, $wsId) 'after'
        Stop-Workspace $conn $wsId $name
        Remove-Workspace $conn $wsId $name
    } finally {
        Stop-Installed
    }

    Invoke-Msiexec @('/x', "`"$Upgrade`"") 'msi-uninstall.log'
    Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the uninstall"
}
