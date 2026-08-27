# v2: single top-to-bottom global z-order dump around the DeskFence pack.
# Prints every top-level window from screen top down to slightly below the
# lowest DeskFence window, plus standalone dumps of ScW / EdgeUiInputTopWndClass.
# ASCII only.
$ErrorActionPreference = 'Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class ZP {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int cb);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
}
"@
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class DPI { [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v); }
"@
[void][DPI]::SetProcessDpiAwarenessContext([IntPtr](-4))

$rows = New-Object System.Collections.ArrayList
$cb = [ZP+EnumProc]{ param($h,$l)
    $sb = New-Object System.Text.StringBuilder 256
    [void][ZP]::GetClassName($h,$sb,256)
    $cls = $sb.ToString()
    $r = New-Object ZP+RECT
    [void][ZP]::GetWindowRect($h,[ref]$r)
    $pid2 = [uint32]0
    [void][ZP]::GetWindowThreadProcessId($h,[ref]$pid2)
    $cloak = -1
    [void][ZP]::DwmGetWindowAttribute($h,14,[ref]$cloak,4)
    $exstyle = [ZP]::GetWindowLong($h,-20)
    [void]$rows.Add(@{
        h=$h; cls=$cls; pid=$pid2;
        vis=[ZP]::IsWindowVisible($h); ico=[ZP]::IsIconic($h);
        l=$r.L; t=$r.T; rt=$r.R; b=$r.B; cloak=$cloak; ex=$exstyle })
    return $true
}
[void][ZP]::EnumWindows($cb,[IntPtr]::Zero)

# annotate process names
for ($i=0; $i -lt $rows.Count; $i++) {
    try { $rows[$i].proc = (Get-Process -Id $rows[$i].pid -ErrorAction Stop).ProcessName } catch { $rows[$i].proc='?' }
}

function ShowRow($row,$idx) {
    Write-Output ("[{0,3}] {1} pid={2} proc={3} rect=({4},{5})-({6},{7}) vis={8} ico={9} cloak={10} ex={11:X}" -f `
      $idx,$row.cls,$row.pid,$row.proc,$row.l,$row.t,$row.rt,$row.b,$row.vis,$row.ico,$row.cloak,$row.ex)
}

Write-Output ("total={0}" -f $rows.Count)
$packLo = -1
for ($i=0; $i -lt $rows.Count; $i++) {
    if ($rows[$i].cls -eq 'DeskFenceFence' -or $rows[$i].cls -eq 'DeskFenceTray') { if ($packLo -lt 0) { $packLo = $i } }
}
if ($packLo -ge 0) {
    $from = [Math]::Max(0, $packLo-14)
    $to   = [Math]::Min($rows.Count-1, $packLo+12)
    Write-Output ("--- context around DeskFence pack (first pack idx={0}, window {1}..{2}; index 0 = topmost) ---" -f $packLo,$from,$to)
    for ($i=$from; $i -le $to; $i++) { ShowRow $rows[$i] $i }
} else {
    Write-Output "no DeskFence windows found"
}

Write-Output "--- anything between the lowest DeskFence and bottom-of-stack that is visible+fullscreen-ish ---"
if ($packLo -ge 0) {
    for ($i=$packLo; $i -lt $rows.Count; $i++) {
        $w = $rows[$i]
        $isBig = (($w.rt-$w.l) -ge 800) -and (($w.b-$w.t) -ge 400)
        if (-not $w.vis -and $w.cloak -le 0 -and -not $isBig) { continue }
        ShowRow $w $i
    }
}
