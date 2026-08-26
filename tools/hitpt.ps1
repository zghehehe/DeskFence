# WindowFromPoint hit-test probe (no input injection, works while locked).
# ASCII only.
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class HP {
  [StructLayout(LayoutKind.Sequential)] public struct PT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(PT p);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
[void][HP]::SetProcessDpiAwarenessContext([IntPtr](-4))
$points = @(
  @(120, 540, "fence1 empty bottom"),
  @(122, 34,  "fence1 icon1"),
  @(60, 300,  "fence1 mid-left"),
  @(300, 300, "fence2 interior"),
  @(700, 300, "gap between fences/desktop"),
  @(1500, 640, "bare desktop right"),
  @(960, 640, "screen center desktop")
)
foreach ($p in $points) {
  $pt = New-Object HP+PT
  $pt.X = $p[0]; $pt.Y = $p[1]
  $h = [HP]::WindowFromPoint($pt)
  $sb = New-Object System.Text.StringBuilder 256
  [void][HP]::GetClassName($h, $sb, 256)
  $procId = 0
  [void][HP]::GetWindowThreadProcessId($h, [ref]$procId)
  "({0},{1}) {2} -> hwnd=0x{3:x} cls={4} pid={5}" -f $p[0], $p[1], $p[2], $h.ToInt64(), $sb.ToString(), $procId
}
