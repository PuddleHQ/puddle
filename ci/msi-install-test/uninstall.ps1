# SPDX-License-Identifier: AGPL-3.0-or-later
# Uninstall phase: the offer to quit a running Puddle and the full-removal option.
# Dot-sourced by ci/msi-install-test.ps1 at script scope: it shares the main script's variables and parameters.

# Puddle processes of any kind, from any folder.
function Get-PuddleProcessesNow { @(Get-Process -Name 'msb', 'puddle-app', 'puddle' -ErrorAction SilentlyContinue) }

# The uninstaller's offer and its full-removal option (main.wxs PuddleRemoveDlg, PuddleStop, PuddleRemoveAll;
# crates/puddle-app/wix/puddle-uninstall.ps1):
#   - with Puddle (and a workspace) running, a silent uninstall still refuses and changes nothing;
#   - msiexec /x (what Windows' app list runs) shows only the basic interface: its Yes/No message says Puddle is
#     running; Yes quits it through its own quit (the workspaces
#     stop cleanly), the uninstall goes on, and the data folder stays;
#   - a silent uninstall asked for everything (PUDDLE_STOP_RUNNING=1 PUDDLE_REMOVE_ALL=1) over a running Puddle
#     leaves no process, no data folder, no shell state, no Include line of Puddle's in the user's ssh config
#     (the rest of that file stays) and nothing new on the machine.
# The uninstall helper's own note of a stop (its session, the sessions of Puddle's processes), into the logs.
function Save-UninstallHelperLog {
    foreach ($dir in @([IO.Path]::GetTempPath(), (Join-Path $env:SystemRoot 'Temp'))) {
        $note = Join-Path $dir 'puddle-uninstall.log'
        if (Test-Path -LiteralPath $note) { Get-Content -LiteralPath $note -ErrorAction SilentlyContinue | Add-Content -LiteralPath (Join-Path $LogDir 'puddle-uninstall.log') }
    }
}

function Test-UninstallRunningApp([string]$Package, [string]$Policy) {
    Step 'an uninstall over a running Puddle is refused unless asked; the dialog offers to quit it and go on'
    Invoke-Msiexec @('/i', "`"$Package`"") 'msi-running-install.log'
    $conn = Start-Installed
    $wsId = $null
    try {
        if ($Policy -eq 'run') {
            $name = 'running-uninstall-test'
            $wsId = New-Workspace $conn $name
            Wait-WorkspaceRunning $conn $wsId $name $CreateBudgetSec | Out-Null
            Expect (@(Get-RuntimeProcesses).Count -gt 0) 'no workspace runtime process runs, so the uninstall below would prove nothing'
        }
        $refused = Invoke-MsiexecRefused @('/x', "`"$Package`"") 'msi-running-uninstall-refused.log'
        Expect ($refused.Code -ne 0) 'a silent uninstall over a running Puddle went ahead; it must refuse unless asked to quit it'
        Expect ($refused.Log.Contains($RefusalText)) "the refused uninstall does not say to quit Puddle; see $(Join-Path $LogDir 'msi-running-uninstall-refused.log')"
        Expect (Test-Path -LiteralPath (Join-Path $InstallDir 'puddle-app.exe')) 'the refused uninstall removed the program'
        Expect ((Get-Process -Name 'puddle-app' -ErrorAction SilentlyContinue).Count -gt 0) 'the refused uninstall ended the running program'

        $ui = Invoke-MsiexecWithUi @('/x', "`"$Package`"") 'msi-running-uninstall-ui.log' 600
        Expect ($ui.Code -eq 0) "the uninstall through its dialogs exited with $($ui.Code); see $($ui.Log)"
        $offers = @($ui.Dialogs | Where-Object { (($_.Texts -join ' ') -replace '\s+', ' ').Contains('Puddle is running') })
        Expect ($offers.Count -gt 0) "no uninstall dialog said Puddle is running and offered to quit it; dialogs: $(($ui.Dialogs | ForEach-Object { $_.Texts -join ' / ' }) -join ' || ')"
        $left = Get-PuddleProcessesNow
        Expect ($left.Count -eq 0) "the uninstall left Puddle processes running: $(($left | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')"
        Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the uninstall through its dialogs"
        Expect (Test-Path -LiteralPath $DataDir) 'the uninstall without the removal box ticked removed the data folder'
        if ($wsId) {
            $kept = @(Get-DataListing | Where-Object { $_.Contains($wsId) })
            Expect ($kept.Count -gt 0) "the uninstall removed the files of workspace $wsId without the removal box ticked"
        }
    } finally {
        Save-UninstallHelperLog
        Stop-Installed
    }

    Step 'a silent uninstall asked to quit Puddle and remove everything leaves nothing of it on the machine'
    $before = @(Get-Footprint)
    Invoke-Msiexec @('/i', "`"$Package`"") 'msi-removeall-install.log'
    $sshConfig = Join-Path $env:USERPROFILE '.ssh\config'
    $sshOriginal = if (Test-Path -LiteralPath $sshConfig) { Get-Content -LiteralPath $sshConfig -Raw } else { $null }
    $sshDir = Split-Path -Parent $sshConfig
    $sshDirExisted = Test-Path -LiteralPath $sshDir
    $conn = Start-Installed
    try {
        # The development certificate's file, with a certificate that is in no store (the removal reads the
        # file, then looks the certificate up by its thumbprint: not found is nothing to remove). Create() takes
        # no argument: PowerShell binds an ECCurve argument to Create(string), which returns null.
        $key = [System.Security.Cryptography.ECDsa]::Create()
        $request = New-Object System.Security.Cryptography.X509Certificates.CertificateRequest('CN=localhost', $key, [System.Security.Cryptography.HashAlgorithmName]::SHA256)
        $cert = $request.CreateSelfSigned([DateTimeOffset]::UtcNow.AddDays(-1), [DateTimeOffset]::UtcNow.AddDays(30))
        $pem = "-----BEGIN CERTIFICATE-----`n" + [Convert]::ToBase64String($cert.RawData, 'InsertLineBreaks') + "`n-----END CERTIFICATE-----`n"
        $certDir = Join-Path $DataDir 'dev-cert'
        New-Item -ItemType Directory -Path $certDir -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $certDir 'localhost.pem') -Value $pem
        New-Item -ItemType Directory -Path (Split-Path -Parent $sshConfig) -Force | Out-Null
        $include = "Include `"$(Join-Path $DataDir 'ssh\config')`""
        Set-Content -LiteralPath $sshConfig -Value @($include, '', 'Host keep-me', '  HostName example.invalid')
        Expect (@(Get-ChildItem -LiteralPath $DataDir -Force).Count -gt 0) 'the data folder is empty, so its removal would prove nothing'
        Invoke-Msiexec @('/x', "`"$Package`"", 'PUDDLE_STOP_RUNNING=1', 'PUDDLE_REMOVE_ALL=1') 'msi-removeall-uninstall.log'
        $left = Get-PuddleProcessesNow
        Expect ($left.Count -eq 0) "the uninstall left Puddle processes running: $(($left | ForEach-Object { "$($_.Name) $($_.Id)" }) -join ', ')"
        Expect (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is still there after the full removal"
        $gone = @($DataDir, (Join-Path $env:LOCALAPPDATA 'be.tijsvankampen.puddle'), (Join-Path $env:APPDATA 'be.tijsvankampen.puddle'))
        foreach ($folder in $gone) {
            Expect (-not (Test-Path -LiteralPath $folder)) "$folder is still there after the full removal"
        }
        $sshAfter = Get-Content -LiteralPath $sshConfig -Raw
        Expect (-not $sshAfter.Contains($include)) "the full removal left Puddle's Include line in $sshConfig"
        Expect ($sshAfter.Contains('Host keep-me')) "the full removal changed the rest of $sshConfig"
        Expect ($null -eq (Get-LoginEntry)) "the full removal left the sign-in entry: $(Get-LoginEntry)"
        # The test planted the ssh config (and ~/.ssh on a runner that had none): that folder is the test's, not
        # Puddle's, so it goes before the footprint is compared.
        if (-not $sshDirExisted) { Remove-Item -LiteralPath $sshDir -Recurse -Force -ErrorAction SilentlyContinue }
        $added = @(Get-Footprint | Where-Object { $before -notcontains $_ })
        Expect ($added.Count -eq 0) "the full removal left: $($added -join '; ')"
        Write-Host 'the full removal left no process, folder, sign-in entry or ssh config line of Puddle'
    } finally {
        Save-UninstallHelperLog
        Stop-Installed
        if ($null -eq $sshOriginal) { Remove-Item -LiteralPath $sshConfig -Force -ErrorAction SilentlyContinue }
        else { Set-Content -LiteralPath $sshConfig -Value $sshOriginal -NoNewline }
    }
}
