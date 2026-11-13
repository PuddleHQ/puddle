# SPDX-License-Identifier: AGPL-3.0-or-later
# Real input for terminal-keys.mjs, through Win32 SendInput, so it takes the path a user's keys take
# (the window's accelerators, the webview's own handling), which DevTools' Input.dispatchKeyEvent
# skips. One action per call:
#   terminal-keys.ps1 click <screenX> <screenY>   a left click at physical screen pixels
#   terminal-keys.ps1 keys ctrl+shift+v           a chord: modifiers down in order, the key, then up in reverse
#   terminal-keys.ps1 clipboard <text>            puts text on the system clipboard
#   terminal-keys.ps1 windows <pid>               lists the process's visible top-level windows (diagnostics)
param([Parameter(Mandatory = $true)][string]$Action, [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest)
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Native {
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION u; }
  [DllImport("user32.dll", SetLastError = true)] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern int GetSystemMetrics(int index);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  // Built here: a nested struct assigned field by field from PowerShell is a copy that is lost.
  public static INPUT KeyInput(int vk, bool up, bool extended) {
    var i = new INPUT(); i.type = 1; i.u.ki.wVk = (ushort)vk;
    i.u.ki.dwFlags = (up ? 2u : 0u) | (extended ? 1u : 0u);
    return i;
  }
  public static INPUT MouseInput(uint flags) { var i = new INPUT(); i.type = 0; i.u.mi.dwFlags = flags; return i; }
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  public static string Windows(uint pid) {
    var lines = new List<string>();
    EnumWindows((h, l) => {
      uint owner; GetWindowThreadProcessId(h, out owner);
      if (owner == pid && IsWindowVisible(h)) {
        var t = new StringBuilder(256); GetWindowText(h, t, 256);
        RECT r; GetWindowRect(h, out r);
        lines.Add(string.Format("{0} \"{1}\" [{2},{3} {4}x{5}]{6}", h, t, r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top, h == GetForegroundWindow() ? " FOREGROUND" : ""));
      }
      return true;
    }, IntPtr.Zero);
    return string.Join("\n", lines);
  }
}
'@

[void][Native]::SetProcessDPIAware()
$VK = @{ ctrl = 0x11; shift = 0x10; alt = 0x12; insert = 0x2D; f5 = 0x74; esc = 0x1B }
$EXTENDED = @('insert')

function Send([object[]]$inputs) {
  $size = [System.Runtime.InteropServices.Marshal]::SizeOf([type][Native+INPUT])
  $sent = [Native]::SendInput([uint32]$inputs.Count, [Native+INPUT[]]$inputs, $size)
  if ($sent -ne $inputs.Count) { throw "SendInput sent $sent of $($inputs.Count): error $([System.Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
}
switch ($Action) {
  'click' {
    [void][Native]::SetCursorPos([int]$Rest[0], [int]$Rest[1])
    Start-Sleep -Milliseconds 200
    Send @([Native]::MouseInput(2), [Native]::MouseInput(4))   # left down, left up
  }
  'keys' {
    $names = $Rest[0].ToLowerInvariant().Split('+')
    $codes = @(); $ext = @()
    foreach ($n in $names) {
      if ($VK.ContainsKey($n)) { $codes += $VK[$n]; $ext += ($EXTENDED -contains $n) }
      elseif ($n -match '^[a-z0-9]$') { $codes += [int][char]$n.ToUpperInvariant(); $ext += $false }
      else { throw "unknown key $n" }
    }
    $seq = @()
    for ($k = 0; $k -lt $codes.Count; $k++) { $seq += [Native]::KeyInput($codes[$k], $false, $ext[$k]) }
    for ($k = $codes.Count - 1; $k -ge 0; $k--) { $seq += [Native]::KeyInput($codes[$k], $true, $ext[$k]) }
    Send $seq
  }
  'clipboard' { Set-Clipboard -Value ($Rest -join ' ') }
  'windows' { [Native]::Windows([uint32]$Rest[0]) }
  default { throw "unknown action $Action" }
}
