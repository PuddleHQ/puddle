# SPDX-License-Identifier: AGPL-3.0-or-later
# Closes one top-level window of puddle-app.exe the way the close button does (WM_CLOSE), found by a title regex.
#   powershell -File window-close.ps1 -Title '^msedit - Puddle$'      prints "closed <title>" (exit 0) or "no window" (exit 1)
# Windows PowerShell 5.1, ASCII only. No process is started, no WMI.
param([Parameter(Mandatory = $true)][string]$Title)
Add-Type @"
using System; using System.Text; using System.Collections.Generic; using System.Runtime.InteropServices;
public class PuddleWin {
  delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  public static List<object[]> Of(int[] pids) {
    var r = new List<object[]>();
    EnumWindows((h, l) => {
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (Array.IndexOf(pids, (int)pid) >= 0 && IsWindowVisible(h)) { var sb = new StringBuilder(256); GetWindowText(h, sb, 256); r.Add(new object[] { h, sb.ToString() }); }
      return true;
    }, IntPtr.Zero);
    return r;
  }
}
"@
$pids = [int[]]@(Get-Process puddle-app -ErrorAction SilentlyContinue | ForEach-Object { $_.Id })
foreach ($w in [PuddleWin]::Of($pids)) {
    if ($w[1] -match $Title) { [void][PuddleWin]::PostMessage([IntPtr]$w[0], 0x0010, [IntPtr]::Zero, [IntPtr]::Zero); Write-Output "closed $($w[1])"; exit 0 }
}
Write-Output 'no window'; exit 1
