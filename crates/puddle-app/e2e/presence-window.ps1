# SPDX-License-Identifier: AGPL-3.0-or-later
# What presence.mjs does to the shell's main window, through plain Win32 calls (the window is found
# by process id; it is the one top-level window with a title bar, hidden or not). One action per call:
#   presence-window.ps1 state <pid>      prints `visible=<True|False> minimized=<True|False>`
#   presence-window.ps1 minimize <pid>   ShowWindow(SW_MINIMIZE), the taskbar button's effect
#   presence-window.ps1 close <pid>      WM_CLOSE, what the title bar's close button sends
param([Parameter(Mandatory = $true)][string]$Action, [Parameter(Mandatory = $true)][uint32]$ProcessId)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class Main {
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int index);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  delegate bool EnumProc(IntPtr h, IntPtr l);
  const int GWL_STYLE = -16;
  const int WS_CAPTION = 0x00C00000;
  public static IntPtr Find(uint pid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      var title = new StringBuilder(256); GetWindowText(h, title, 256);
      if (owner == pid && title.Length > 0 && (GetWindowLong(h, GWL_STYLE) & WS_CAPTION) == WS_CAPTION) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@

$window = [Main]::Find($ProcessId)
if ($window -eq [IntPtr]::Zero) { throw "no main window for process $ProcessId" }
switch ($Action) {
  'state' { "visible=$([Main]::IsWindowVisible($window)) minimized=$([Main]::IsIconic($window))" }
  'minimize' { [void][Main]::ShowWindow($window, 6) }
  'close' { [void][Main]::PostMessage($window, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) }
  default { throw "unknown action $Action" }
}
