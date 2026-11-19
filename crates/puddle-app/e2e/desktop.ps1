# SPDX-License-Identifier: AGPL-3.0-or-later
# The Windows shell around the installed app, for usable.mjs: what a person sees and does outside the
# page. One action per call, one JSON line (or one word) out:
#   desktop.ps1 toasts <AppUserModelID>              the app's notifications in the Action Center: [{title, body}]
#   desktop.ps1 toasts-clear <AppUserModelID>        removes them from the Action Center
#   desktop.ps1 window <pid> <titleRegex> <op>       a top-level window of the process, hidden ones too; op is
#                                                    state, front, minimize or close (the title bar's buttons send
#                                                    the same WM_SYSCOMMAND); prints {found, visible, minimized, foreground}
#   desktop.ps1 clipboard-set <text>                 puts text on the clipboard
#   desktop.ps1 clipboard-get                        the clipboard's text, as a JSON string
#   desktop.ps1 processes <name>...                  the running processes of those names: [{name, id}]
#   desktop.ps1 tray-quit                            the tray icon's "Quit Puddle": prints "quit chosen", or "no tray icon" (exit 1)
# Windows PowerShell 5.1, ASCII only. No process is started, no WMI.
param([Parameter(Mandatory = $true)][string]$Action, [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Shell {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, int x, int y, uint d, int e);
  // The process's top-level windows whose title matches, visible ones first.
  public static List<IntPtr> Find(uint pid, string pattern) {
    var shown = new List<IntPtr>(); var hidden = new List<IntPtr>();
    var re = new System.Text.RegularExpressions.Regex(pattern);
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner != pid) return true;
      var t = new StringBuilder(512); GetWindowText(h, t, 512);
      if (t.Length == 0 || !re.IsMatch(t.ToString())) return true;
      (IsWindowVisible(h) ? shown : hidden).Add(h);
      return true;
    }, IntPtr.Zero);
    shown.AddRange(hidden);
    return shown;
  }
}
'@

function Window-State([IntPtr]$h) {
  return [ordered]@{
    found = $true
    visible = [Shell]::IsWindowVisible($h)
    minimized = [Shell]::IsIconic($h)
    foreground = ([Shell]::GetForegroundWindow() -eq $h)
  }
}

switch ($Action) {
  'toasts' {
    $null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
    $items = @()
    foreach ($t in [Windows.UI.Notifications.ToastNotificationManager]::History.GetHistory($Rest[0])) {
      $texts = @()
      foreach ($node in $t.Content.GetElementsByTagName('text')) { $texts += [string]$node.InnerText }
      $items += [ordered]@{ title = $(if ($texts.Count -gt 0) { $texts[0] } else { '' }); body = (@($texts | Select-Object -Skip 1 | Where-Object { $_ }) -join "`n") }
    }
    ConvertTo-Json -InputObject @($items) -Compress -Depth 4
  }
  'toasts-clear' {
    $null = [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
    [Windows.UI.Notifications.ToastNotificationManager]::History.Clear($Rest[0])
    'cleared'
  }
  'window' {
    $found = [Shell]::Find([uint32]$Rest[0], $Rest[1])
    if ($found.Count -eq 0) { ConvertTo-Json -InputObject @{ found = $false } -Compress; break }
    $h = $found[0]
    $WM_SYSCOMMAND = 0x0112
    switch ($Rest[2]) {
      'state' {}
      'close' { [void][Shell]::PostMessage($h, $WM_SYSCOMMAND, [IntPtr]0xF060, [IntPtr]::Zero) }
      'minimize' { [void][Shell]::PostMessage($h, $WM_SYSCOMMAND, [IntPtr]0xF020, [IntPtr]::Zero) }
      'front' {
        if ([Shell]::IsIconic($h)) { [void][Shell]::ShowWindow($h, 9) }
        # A process may take the foreground only after input of its own; an Alt press is that input. A window that is
        # in front already needs none, and the Alt press would reach its page: WebView2 then stops answering UI Automation
        # (every FindAll times out) once the page has no focused control (T-1961).
        if ([Shell]::GetForegroundWindow() -ne $h) {
          [Shell]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
          [Shell]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
          [void][Shell]::SetForegroundWindow($h)
        }
      }
      default { throw "unknown window operation $($Rest[2])" }
    }
    Start-Sleep -Milliseconds 500
    ConvertTo-Json -InputObject (Window-State $h) -Compress
  }
  'clipboard-set' { Set-Clipboard -Value $Rest[0]; 'set' }
  'clipboard-get' { ConvertTo-Json -InputObject ([string](Get-Clipboard -Raw)) -Compress }
  'processes' {
    $items = @(Get-Process -Name $Rest -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ name = $_.Name; id = $_.Id } })
    ConvertTo-Json -InputObject @($items) -Compress
  }
  'tray-quit' {
    # The tray icon's menu: a disabled count line, Open Puddle, Quit Puddle. The popup is a native menu UI
    # Automation does not list, so it is opened with a real right click and the last entry is picked with
    # the keyboard (Up from nothing selected lands on it).
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
    [void][Shell]::SetProcessDPIAware()
    $A = [System.Windows.Automation.AutomationElement]
    $root = $A::RootElement
    $buttons = New-Object System.Windows.Automation.PropertyCondition($A::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    function Find-Icon {
      foreach ($e in $root.FindAll('Descendants', $buttons)) {
        if ($e.Current.Name -match '^Puddle' -and $e.Current.ClassName -eq 'SystemTray.NormalButton') { return $e }
      }
      return $null
    }
    $icon = Find-Icon
    if (-not $icon) {
      $tb = $root.FindFirst('Children', (New-Object System.Windows.Automation.PropertyCondition($A::ClassNameProperty, 'Shell_TrayWnd')))
      $more = $tb.FindAll('Descendants', (New-Object System.Windows.Automation.PropertyCondition($A::NameProperty, 'Show Hidden Icons'))) | Select-Object -First 1
      if ($more) { $more.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Seconds 2 }
      $icon = Find-Icon
    }
    if (-not $icon) { 'no tray icon'; exit 1 }
    $r = $icon.Current.BoundingRectangle
    [void][Shell]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2))
    Start-Sleep -Milliseconds 400
    [Shell]::mouse_event(8, 0, 0, 0, 0); [Shell]::mouse_event(16, 0, 0, 0, 0)
    Start-Sleep -Seconds 2
    [System.Windows.Forms.SendKeys]::SendWait('{UP}')
    Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
    'quit chosen'
  }
  default { throw "unknown action $Action" }
}
