# SPDX-License-Identifier: AGPL-3.0-or-later
# The Windows UI Automation side of uia.mjs: one long-lived PowerShell that answers one JSON request per
# input line with one JSON line. It finds the app's top-level windows by process id, finds elements in
# the web content WebView2 exposes through UI Automation, and acts on them (Invoke, Toggle, Select, a
# value, or a real mouse click when no pattern exists). Windows PowerShell 5.1 only (System.Windows.Automation).
param([Parameter(Mandatory)][int]$AppProcessId)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public class UiaMouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, UIntPtr e);
  public static void Click(int x, int y) { SetCursorPos(x, y); mouse_event(2, 0, 0, 0, UIntPtr.Zero); mouse_event(4, 0, 0, 0, UIntPtr.Zero); }
}
"@
$AE = [System.Windows.Automation.AutomationElement]
$CT = [System.Windows.Automation.ControlType]
$Scope = [System.Windows.Automation.TreeScope]
$roles = @{
  button = $CT::Button; radio = $CT::RadioButton; checkbox = $CT::CheckBox; link = $CT::Hyperlink
  text = $CT::Text; edit = $CT::Edit; listitem = $CT::ListItem; window = $CT::Window; pane = $CT::Pane
  group = $CT::Group; list = $CT::List; tab = $CT::TabItem; document = $CT::Document; tree = $CT::Tree; treeitem = $CT::TreeItem
}

function Get-Windows([string]$titleRegex) {
  $children = $AE::RootElement.FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition)
  $found = @()
  foreach ($w in $children) {
    if ($w.Current.ProcessId -ne $AppProcessId) { continue }
    if ($titleRegex -and ($w.Current.Name -notmatch $titleRegex)) { continue }
    $found += $w
  }
  return $found
}

function Get-Matches($req) {
  $result = @()
  foreach ($win in (Get-Windows $req.window)) {
    $conds = @()
    if ($req.role) { $conds += [System.Windows.Automation.PropertyCondition]::new($AE::ControlTypeProperty, $roles[$req.role]) }
    if ($req.name) { $conds += [System.Windows.Automation.PropertyCondition]::new($AE::NameProperty, [string]$req.name) }
    $cond = [System.Windows.Automation.Condition]::TrueCondition
    if ($conds.Count -eq 1) { $cond = $conds[0] } elseif ($conds.Count -eq 2) { $cond = [System.Windows.Automation.AndCondition]::new($conds[0], $conds[1]) }
    foreach ($e in $win.FindAll($Scope::Descendants, $cond)) {
      if ($req.nameRegex -and ($e.Current.Name -notmatch $req.nameRegex)) { continue }
      if ($req.localized -and ($e.Current.LocalizedControlType -ne $req.localized)) { continue }
      $result += $e
    }
  }
  return $result
}

function Describe($e) {
  $c = $e.Current
  $state = $null
  $p = $null
  if ($e.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$p)) { $state = ($p.Current.ToggleState -eq 'On') }
  elseif ($e.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $state = $p.Current.IsSelected }
  return [ordered]@{ name = $c.Name; type = $c.ControlType.ProgrammaticName -replace '^ControlType\.', ''; localized = $c.LocalizedControlType; enabled = $c.IsEnabled; offscreen = $c.IsOffscreen; checked = $state }
}

function Pick($req) {
  $all = @(Get-Matches $req)
  $usable = @($all | Where-Object { $_.Current.IsEnabled -and -not $_.Current.IsOffscreen })
  if ($usable.Count -gt 0) { return $usable[0] }
  $enabled = @($all | Where-Object { $_.Current.IsEnabled })
  if ($enabled.Count -gt 0) { return $enabled[0] }
  return $null
}

function Click-Element($e) {
  $p = $null
  if ($e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$p)) { $p.Invoke(); return 'invoke' }
  if ($e.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$p)) { $p.Select(); return 'select' }
  if ($e.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$p)) { $p.Toggle(); return 'toggle' }
  $r = $e.Current.BoundingRectangle
  if ($r.IsEmpty) { throw 'the element has no pattern and no position to click' }
  [UiaMouse]::Click([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
  return 'mouse'
}

function Handle($req) {
  switch ($req.action) {
    'windows' { return @{ items = @(Get-Windows $req.window | ForEach-Object { Describe $_ }) } }
    'find' { return @{ items = @(Get-Matches $req | Select-Object -First 50 | ForEach-Object { Describe $_ }) } }
    'click' {
      $e = Pick $req
      if (-not $e) { return @{ found = $false } }
      return @{ found = $true; how = (Click-Element $e); element = (Describe $e) }
    }
    'value' {
      $e = Pick $req
      if (-not $e) { return @{ found = $false } }
      $p = $null
      $value = $null
      if ($e.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$p)) { $value = $p.Current.Value }
      return @{ found = $true; value = $value; element = (Describe $e) }
    }
    'set' {
      $e = Pick $req
      if (-not $e) { return @{ found = $false } }
      $p = $null
      $how = 'none'
      if ($e.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$p)) {
        $p.SetValue([string]$req.value)
        $how = 'value-pattern'
        if ($p.Current.Value -ne [string]$req.value) {
          # Typed as a person types, when the control refused the pattern's value.
          $e.SetFocus()
          [System.Windows.Forms.SendKeys]::SendWait('^a{DEL}')
          $escaped = ([string]$req.value) -replace '([+^%~(){}\[\]])', '{$1}'
          [System.Windows.Forms.SendKeys]::SendWait($escaped)
          $how = 'typed'
        }
        return @{ found = $true; how = $how; value = $p.Current.Value; element = (Describe $e) }
      }
      throw 'the element has no value pattern'
    }
    'dump' {
      # The names under the windows, for a failure's diagnosis (depth-first, capped).
      $lines = @()
      foreach ($win in (Get-Windows $req.window)) {
        $lines += "WINDOW $($win.Current.Name)"
        foreach ($e in $win.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
          if ($lines.Count -gt 600) { break }
          $c = $e.Current
          if ($c.Name) { $lines += ('{0}/{1} | {2}' -f ($c.ControlType.ProgrammaticName -replace '^ControlType\.', ''), $c.LocalizedControlType, $c.Name) }
        }
      }
      return @{ lines = $lines }
    }
    default { throw "unknown action $($req.action)" }
  }
}

[Console]::Out.WriteLine('{"ready":true}')
[Console]::Out.Flush()
while ($null -ne ($line = [Console]::In.ReadLine())) {
  try {
    $res = Handle ($line | ConvertFrom-Json)
    $res.ok = $true
  } catch {
    $res = @{ ok = $false; error = "$_" }
  }
  [Console]::Out.WriteLine((ConvertTo-Json -InputObject $res -Compress -Depth 6))
  [Console]::Out.Flush()
}
