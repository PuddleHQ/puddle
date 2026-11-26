# SPDX-License-Identifier: AGPL-3.0-or-later
# Run by the installer's uninstall (crates/puddle-app/wix/main.wxs), never by hand.
#   -Stop       asks the running Puddle to quit (`puddle-app.exe --quit`, the program's own quit: it
#               stops every workspace cleanly) and waits for its processes to end. It never kills one.
#   -RemoveAll  deletes what Puddle keeps outside the install folder: the data folder (workspaces,
#               disks, settings, logs, the runtime's home), the shell's own state, the development
#               certificate it trusted, and its Include line in ~/.ssh/config.
# The list shown to the user in the uninstall dialog is the list below, nothing else is touched.
param(
    [switch]$Stop,
    [switch]$RemoveAll,
    [string]$InstallDir = (Split-Path -Parent (Split-Path -Parent $PSCommandPath)),
    [int]$WaitSeconds = 300
)
$ErrorActionPreference = 'Continue'

function Get-PuddleProcesses { @(Get-Process -Name 'puddle-app', 'msb' -ErrorAction SilentlyContinue) }

if ($Stop) {
    $app = Join-Path $InstallDir 'puddle-app.exe'
    if ((Get-PuddleProcesses).Count -gt 0 -and (Test-Path -LiteralPath $app)) {
        & $app --quit
        $deadline = (Get-Date).AddSeconds($WaitSeconds)
        while ((Get-PuddleProcesses).Count -gt 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Seconds 1 }
    }
}

if ($RemoveAll) {
    $data = Join-Path $env:LOCALAPPDATA 'puddle'
    # The trusted development certificate first: it is found through the file in the data folder.
    $pem = Join-Path $data 'dev-cert\localhost.pem'
    if (Test-Path -LiteralPath $pem) {
        $match = [regex]::Match((Get-Content -Raw -LiteralPath $pem), '-----BEGIN CERTIFICATE-----(.+?)-----END CERTIFICATE-----', 'Singleline')
        if ($match.Success) {
            $bytes = [Convert]::FromBase64String(($match.Groups[1].Value -replace '\s', ''))
            $cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 (, $bytes)
            $entry = Join-Path 'Cert:\CurrentUser\Root' $cert.Thumbprint
            if (Test-Path -LiteralPath $entry) { Remove-Item -LiteralPath $entry -Force }
        }
    }
    # Puddle's line in the user's own ssh config (`Include "<data folder>\ssh\config"`), nothing else of it.
    $sshConfig = Join-Path $env:USERPROFILE '.ssh\config'
    if (Test-Path -LiteralPath $sshConfig) {
        $mine = Join-Path $data 'ssh\config'
        $lines = @(Get-Content -LiteralPath $sshConfig)
        $kept = @($lines | Where-Object { -not ($_ -match '^\s*Include\s' -and $_.Contains($mine)) })
        if ($kept.Count -ne $lines.Count) { Set-Content -LiteralPath $sshConfig -Value $kept }
    }
    $folders = @($data, (Join-Path $env:LOCALAPPDATA 'be.tijsvankampen.puddle'), (Join-Path $env:APPDATA 'be.tijsvankampen.puddle'))
    foreach ($folder in $folders) {
        if (Test-Path -LiteralPath $folder) { Remove-Item -LiteralPath $folder -Recurse -Force }
    }
}
exit 0
