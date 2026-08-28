$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class BW2 {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint c);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr p, IntPtr c, string cls, string cap);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out int v, int cb);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
    public const uint GW_HWNDPREV = 3;
}
"@
[void][BW2]::SetProcessDpiAwarenessContext([IntPtr](-4))
$cands=New-Object System.Collections.ArrayList
$cb=[BW2+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][BW2]::GetClassName($h,$sb,64)
    $cls=$sb.ToString()
    if ($cls -eq 'WorkerW' -or $cls -eq 'Progman') { [void]$cands.Add(@{h=$h;cls=$cls}) }
    return $true }
[void][BW2]::EnumWindows($cb,[IntPtr]::Zero)
$hostH=[IntPtr]::Zero
foreach ($w in $cands) {
    if ([BW2]::FindWindowExW($w.h,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostH=$w.h; break }
}
Write-Output ("host=0x{0:X} ({1})" -f $hostH.ToInt64(), ($cands | Where-Object {$_.h -eq $hostH} | Select-Object -First 1).cls)
# also list ALL DeskFenceFence windows with vis state
$mine=New-Object System.Collections.ArrayList
$cb2=[BW2+EnumProc]{ param($h,$l)
    $sb=New-Object System.Text.StringBuilder 64
    [void][BW2]::GetClassName($h,$sb,64)
    if ($sb.ToString() -like 'DeskFence*') {
        $r=New-Object BW2+RECT
        [void][BW2]::GetWindowRect($h,[ref]$r)
        [void]$mine.Add(@{h=$h;cls=$sb.ToString();vis=[BW2]::IsWindowVisible($h);rect=("({0},{1})-({2},{3})" -f $r.L,$r.T,$r.R,$r.B)})
    }
    return $true }
[void][BW2]::EnumWindows($cb2,[IntPtr]::Zero)
Write-Output ("all DeskFence* windows: {0}" -f $mine.Count)
foreach ($m in $mine) { Write-Output ("  0x{0:X} {1} vis={2} rect={3}" -f $m.h.ToInt64(),$m.cls,$m.vis,$m.rect) }
# walk up to 500 entries, report own/visible landmarks
$cur=[BW2]::GetWindow($hostH,[BW2]::GW_HWNDPREV); $i=0; $invis=0; $vis=0; $own=0
$mineH = @{}; foreach ($m in $mine) { $mineH[$m.h.ToInt64()]=$m }
$visShown=0
while ($cur -ne [IntPtr]::Zero -and $i -lt 500) {
    $i++
    if ($mineH.ContainsKey($cur.ToInt64())) { $own++; Write-Output (" step {0}: OWN {1} 0x{2:X} vis={3}" -f $i,$mineH[$cur.ToInt64()].cls,$cur.ToInt64(),$mineH[$cur.ToInt64()].vis) }
    else {
        $v=[BW2]::IsWindowVisible($cur)
        if ($v) { $vis++; if ($visShown -lt 5) { $sb=New-Object System.Text.StringBuilder 64; [void][BW2]::GetClassName($cur,$sb,64); $r=New-Object BW2+RECT; [void][BW2]::GetWindowRect($cur,[ref]$r); Write-Output (" step {0}: VIS foreign {1} 0x{2:X} rect=({3},{4})-({5},{6})" -f $i,$sb.ToString(),$cur.ToInt64(),$r.L,$r.T,$r.R,$r.B); $visShown++ } }
        else { $invis++ }
    }
    $cur=[BW2]::GetWindow($cur,[BW2]::GW_HWNDPREV)
}
Write-Output ("total={0} own={1} visibleForeign={2} invisible={3}" -f $i,$own,$vis,$invis)
