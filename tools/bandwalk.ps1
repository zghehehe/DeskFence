# Walk the REAL desktop band exactly like ensure_all_attached does:
# from the defview-bearing host upward via GW_HWNDPREV.
$ErrorActionPreference='Continue'
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class BW {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    public delegate bool CB(IntPtr h, IntPtr l);
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
[void][BW]::SetProcessDpiAwarenessContext([IntPtr](-4))
$cands = New-Object System.Collections.ArrayList
$cb = [BW+EnumProc]{ param($h,$l)
    $sb = New-Object System.Text.StringBuilder 64
    [void][BW]::GetClassName($h,$sb,64)
    $cls = $sb.ToString()
    if ($cls -eq 'WorkerW' -or $cls -eq 'Progman') { [void]$cands.Add(@{h=$h;cls=$cls}) }
    return $true }
[void][BW]::EnumWindows($cb,[IntPtr]::Zero)
$hostH = [IntPtr]::Zero
foreach ($w in $cands) {
    if ([BW]::FindWindowExW($w.h,[IntPtr]::Zero,'SHELLDLL_DefView',$null) -ne [IntPtr]::Zero) { $hostH=$w.h; break }
}
if ($hostH -eq [IntPtr]::Zero) { Write-Output 'no host'; exit 1 }
Write-Output ("host=0x{0:X} ({1})" -f $hostH.ToInt64(), ($cands | Where-Object {$_.h -eq $hostH}).cls)
$cur=[BW]::GetWindow($hostH,[BW]::GW_HWNDPREV); $i=0
while ($cur -ne [IntPtr]::Zero -and $i -lt 30) {
    $i++
    $sb=New-Object System.Text.StringBuilder 96
    [void][BW]::GetClassName($cur,$sb,96)
    $r=New-Object BW+RECT
    [void][BW]::GetWindowRect($cur,[ref]$r)
    $pid2=[uint32]0
    [void][BW]::GetWindowThreadProcessId($cur,[ref]$pid2)
    try { $pn=(Get-Process -Id $pid2 -ErrorAction Stop).ProcessName } catch { $pn='?' }
    $cl=-1
    [void][BW]::DwmGetWindowAttribute($cur,14,[ref]$cl,4)
    Write-Output ("+{0,2} {1} proc={2} rect=({3},{4})-({5},{6}) vis={7} ico={8} cloak={9}" -f `
        $i,$sb.ToString(),$pn,$r.L,$r.T,$r.R,$r.B,[BW]::IsWindowVisible($cur),[BW]::IsIconic($cur),$cl)
    $cur=[BW]::GetWindow($cur,[BW]::GW_HWNDPREV)
}
