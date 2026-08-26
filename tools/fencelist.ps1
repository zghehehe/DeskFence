# List top-level windows of the deskfence.exe process with rects. ASCII only.
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WL {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr v);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@
[void][WL]::SetProcessDpiAwarenessContext([IntPtr](-4))
$out = New-Object System.Collections.ArrayList
$cb = {
  param([IntPtr]$h, [IntPtr]$l)
  $pid2 = 0
  [void][WL]::GetWindowThreadProcessId($h, [ref]$pid2)
  if ($pid2 -eq $args -or $true) { }
  return $true
}
# simpler: enumerate all, filter by pid inside script scope
$targetPid = 0
Get-Process deskfence -ErrorAction SilentlyContinue | ForEach-Object { $targetPid = $_.Id }
"deskfence pid = $targetPid"
$cb2 = {
  param([IntPtr]$h, [IntPtr]$l)
  $p = 0
  [void][WL]::GetWindowThreadProcessId($h, [ref]$p)
  if ($p -eq $script:targetPid) {
    $sb = New-Object System.Text.StringBuilder 256
    [void][WL]::GetClassName($h, $sb, 256)
    $r = New-Object WL+RECT
    [void][WL]::GetWindowRect($h, [ref]$r)
    $vis = [WL]::IsWindowVisible($h)
    [void]$script:out.Add(("hwnd=0x{0:x} cls={1} rect=({2},{3})-({4},{5}) {6}x{7} vis={8}" -f $h.ToInt64(), $sb.ToString(), $r.L, $r.T, $r.R, $r.B, ($r.R-$r.L), ($r.B-$r.T), $vis))
  }
  return $true
}
[WL]::EnumWindows($cb2, [IntPtr]::Zero) | Out-Null
$out | ForEach-Object { $_ }
