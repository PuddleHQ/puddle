# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
Reads and answers the Windows certificate-trust "Security Warning" dialog that a process opens when it
adds a certificate to CurrentUser\Root.

.DESCRIPTION
Windows asks for one confirmation when a program adds a root to the current user's store, and the add
waits for it. On a throwaway CI runner nobody clicks, so a test starts this script next to the add and
it presses the button the test names. It uses UI Automation only: no process is started and WMI is
not used. A dialog that never appears is a failure (exit 1), never a pass.

  -Action Dump   print the dialog's texts and buttons, press nothing
  -Action Yes    print them, then press Yes
  -Action No     print them, then press No
  -OwnerPid      only a dialog owned by this process counts (the process that makes the add)
  -Seconds       how long to wait for the dialog to appear
  -Optional      no dialog is not a failure (exit 0 and "no dialog"): for a step that may or may not ask

Output lines the caller reads: `dialog: '<title>' owned by pid <n>`, one `  <ControlType>: <text>` line per
element, and `pressed Yes` or `pressed No`. Exit: 0 done, 1 no dialog within -Seconds, 2 no such button.

Windows PowerShell 5.1 and pwsh; keep this file ASCII-only.
#>
param(
    [ValidateSet('Dump', 'Yes', 'No')][string]$Action = 'Dump',
    [Parameter(Mandatory = $true)][int]$OwnerPid,
    [int]$Seconds = 45,
    [string]$Title = 'Security Warning',
    [switch]$Optional
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

function Find-Dialog {
    $root = [Windows.Automation.AutomationElement]::RootElement
    $name = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::NameProperty, $Title)
    $owner = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ProcessIdProperty, $OwnerPid)
    $both = New-Object Windows.Automation.AndCondition($name, $owner)
    return $root.FindFirst([Windows.Automation.TreeScope]::Children, $both)
}

$clock = [Diagnostics.Stopwatch]::StartNew()
$dialog = $null
while ($true) {
    $dialog = Find-Dialog
    if ($dialog -or $clock.Elapsed.TotalSeconds -ge $Seconds) { break }
    Start-Sleep -Milliseconds 250
}
if (-not $dialog) {
    if ($Optional) { Write-Output 'no dialog'; exit 0 }
    $others = $root = [Windows.Automation.AutomationElement]::RootElement
    $titles = @($others.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { "'{0}' (pid {1})" -f $_.Current.Name, $_.Current.ProcessId })
    Write-Output ("NO DIALOG titled '{0}' owned by pid {1} within {2} s; top-level windows: {3}" -f $Title, $OwnerPid, $Seconds, ($titles -join ', '))
    exit 1
}

Write-Output ("dialog: '{0}' owned by pid {1}" -f $dialog.Current.Name, $dialog.Current.ProcessId)
$buttons = @{}
foreach ($e in $dialog.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)) {
    $text = $e.Current.Name
    if ($text) { Write-Output ('  {0}: {1}' -f $e.Current.ControlType.ProgrammaticName.Replace('ControlType.', ''), $text) }
    if ($e.Current.ControlType -eq [Windows.Automation.ControlType]::Button) { $buttons[$text] = $e }
}
if ($Action -ne 'Dump') {
    $wanted = $buttons.Keys | Where-Object { $_ -match ('^(&)?' + $Action + '$') } | Select-Object -First 1
    if (-not $wanted) {
        Write-Output ('no "{0}" button; buttons: {1}' -f $Action, (@($buttons.Keys) -join ', '))
        exit 2
    }
    $buttons[$wanted].GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    Write-Output ('pressed {0}' -f $Action)
}
exit 0
