# Dump the native desktop window chain: z-order, parent/child, styles.
# Pure ASCII (no BOM needed). PS 5.1 + C#5 compatible.
Add-Type -TypeDefinition @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc cb, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint ga);
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
"@

function ClassName([IntPtr]$h) {
  $sb = New-Object System.Text.StringBuilder 256
  [void][W]::GetClassName($h, $sb, 256)
  return $sb.ToString()
}

function DumpWin([IntPtr]$h) {
  $r = New-Object W+RECT
  [void][W]::GetWindowRect($h, [ref]$r)
  $root = [W]::GetAncestor($h, 2)   # GA_ROOT
  $parent = [W]::GetAncestor($h, 1) # GA_PARENT
  $style = [W]::GetWindowLong($h, -16)
  $ex = [W]::GetWindowLong($h, -20)
  $wsChild = ($style -band 0x40000000) -ne 0
  $exLayered = ($ex -band 0x80000) -ne 0
  $vis = [W]::IsWindowVisible($h)
  $parentInfo = if ($parent -eq [IntPtr]::Zero) { "none" } else { ("0x{0:x}:{1}" -f $parent.ToInt64(), (ClassName $parent)) }
  "hwnd=0x{0:x} cls={1} rect=({2},{3})-({4},{5}) vis={6} ws_child={7} ex_layered={8} parent={9}" -f `
    $h.ToInt64(), (ClassName $h), $r.L, $r.T, $r.R, $r.B, $vis, $wsChild, $exLayered, $parentInfo
}

# 1) All top-level windows in z-order (EnumWindows = top to bottom).
$tops = New-Object System.Collections.ArrayList
$cbTop = {
  param([IntPtr]$h, [IntPtr]$l)
  [void]$script:tops.Add($h)
  return $true
}
[W]::EnumWindows($cbTop, [IntPtr]::Zero) | Out-Null

"=== TOP-LEVEL, z-order top to bottom (desktop-relevant classes) ==="
foreach ($h in $tops) {
  $cls = ClassName $h
  if ($cls -in @("Progman", "WorkerW", "SHELLDLL_DefView", "SysListView32")) {
    DumpWin $h
  }
}

"=== ALL DESCENDANTS of each Progman / WorkerW ==="
foreach ($h in $tops) {
  $cls = ClassName $h
  if ($cls -in @("Progman", "WorkerW")) {
    ""
    "--- under $cls (0x{0:x}) ---" -f $h.ToInt64()
    $kids = New-Object System.Collections.ArrayList
    $cbKid = {
      param([IntPtr]$c, [IntPtr]$l)
      [void]$script:kids.Add($c)
      return $true
    }
    [W]::EnumChildWindows($h, $cbKid, [IntPtr]::Zero) | Out-Null
    foreach ($k in $kids) { DumpWin $k }
  }
}
